//! Stable Yield pool — Solana binding layer.
//!
//! The rolling pool: open-ended, holders enter and leave at NAV, and the SPV invests on
//! its own schedule. Every protocol decision comes from `piron-core`.
//!
//! Three things this pool needs that the others do not, and what each costs here:
//!
//! - **NAV is stored, not computed on read.** Solana has no view functions, so `total_nav`
//!   is a field written after every mutation. A client recomputing it would eventually
//!   disagree with what the program believed when it moved money.
//! - **Capital in flight counts toward NAV.** Cash drawn by the SPV has left the vault but
//!   still belongs to the pool. Instruments, vault and in-flight sum to NAV, and every
//!   transition moves value between those three lines rather than creating any.
//! - **The withdrawal queue lives in its own account.** An account created through a CPI
//!   caps at 10,240 bytes and 256 requests need 20,801, so the queue is created at the cap
//!   and grown by realloc. That limit exists on no other chain Piron runs on.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};

pub mod seeds;
pub mod state;

use piron_core::stable_yield::{nav, queue as core_queue, validate};
use state::{
    to_u64, HolderRecord, StableYieldPool, StoredInstrument, StoredRequest, WithdrawalQueue,
    MAX_INSTRUMENTS,
};

declare_id!("PironStab1e11111111111111111111111111111111");

fn map_err(err: piron_core::Error) -> Error {
    Error::from(ProgramError::Custom(err.code()))
}

/// Conditions the binding decides rather than core, numbered to match the Soroban
/// manager's own codes so the same protocol condition reports the same number on both
/// chains — which is what the backend maps on.
mod code {
    pub const UNAUTHORIZED: u32 = 701;
    pub const TOO_MANY_INSTRUMENTS: u32 = 706;
    pub const INSTRUMENT_NOT_FOUND: u32 = 707;
    pub const EXCEEDS_IN_FLIGHT: u32 = 708;
    pub const INVALID_COUPON_RATE: u32 = 709;
    pub const INVALID_FACE_VALUE: u32 = 710;
    pub const QUEUE_FULL: u32 = 711;
    pub const WRONG_QUEUE: u32 = 712;
    pub const HOLDING_PERIOD: u32 = 713;
    pub const OWNER_MISMATCH: u32 = 714;
    pub const QUEUE_NOT_EMPTY: u32 = 715;
    pub const ALREADY_REGISTERED: u32 = 716;
}

fn local(c: u32) -> Error {
    Error::from(ProgramError::Custom(c))
}
fn clock_now() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| map_err(piron_core::Error::Overflow))
}

#[program]
pub mod piron_stable_yield {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        pool_id: u64,
        spv: Pubkey,
        treasury: Pubkey,
        min_investment: u64,
        holding_period: i64,
        withdrawal_fee_bps: u32,
        min_absolute_reserve: u64,
        reserve_ratio_bps: u32,
    ) -> Result<()> {
        let pool_key = ctx.accounts.pool.key();
        let queue_key = ctx.accounts.queue.key();
        {
            let mut queue = ctx.accounts.queue.load_init()?;
            queue.pool = pool_key;
        }
        {
            // `load_init` rather than `load_mut`: the account was just created and its
            // discriminator is not written yet.
            let mut pool = ctx.accounts.pool.load_init()?;
            pool.pool_id = pool_id;
            pool.authority = ctx.accounts.authority.key();
            pool.spv = spv;
            pool.treasury = treasury;
            pool.asset_mint = ctx.accounts.asset_mint.key();
            pool.vault = ctx.accounts.vault.key();
            pool.share_mint = ctx.accounts.share_mint.key();
            pool.queue = queue_key;
            pool.min_investment = min_investment;
            pool.holding_period = holding_period;
            pool.withdrawal_fee_bps = withdrawal_fee_bps;
            pool.min_absolute_reserve = min_absolute_reserve;
            pool.reserve_ratio_bps = reserve_ratio_bps;
            pool.is_active = 1;
            pool.bump = ctx.bumps.pool;
        }
        emit!(PoolInitialized { pool: pool_key, pool_id });
        Ok(())
    }

    /// Buys into the pool at today's price.
    pub fn deposit(ctx: Context<Deposit>, pool_id: u64, assets: u64) -> Result<u64> {
        let _ = pool_id;
        let now = clock_now()?;
        let (min_investment, active, nav_now, bump, id) = {
            let pool = ctx.accounts.pool.load()?;
            let queue = ctx.accounts.queue.load()?;
            (
                pool.min_investment,
                pool.is_active != 0,
                net_nav(&pool, &queue, ctx.accounts.vault.amount, now)?,
                pool.bump,
                pool.pool_id,
            )
        };
        validate::validate_deposit(assets as i128, min_investment as i128, active).map_err(map_err)?;

        let supply = ctx.accounts.share_mint.supply as i128;
        let price = nav::nav_per_share(nav_now, supply).map_err(map_err)?;
        let shares =
            to_u64(nav::shares_for_deposit(assets as i128, price).map_err(map_err)?).map_err(map_err)?;

        token::transfer(ctx.accounts.holder_to_vault(), assets)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::mint_to(ctx.accounts.mint_shares().with_signer(signer), shares)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        // The vault has grown by `assets`; NAV follows from the balance, not a counter.
        refresh_nav(&mut pool, ctx.accounts.vault.amount + assets, now)?;
        drop(pool);

        // The holding period runs from the last buy-in, so topping up restarts it.
        let record = &mut ctx.accounts.holder_record;
        record.pool = ctx.accounts.pool.key();
        record.owner = ctx.accounts.holder.key();
        record.last_deposit_at = now as i64;
        record.bump = ctx.bumps.holder_record;

        emit!(Deposited {
            pool: ctx.accounts.pool.key(),
            investor: ctx.accounts.holder.key(),
            assets,
            shares,
        });
        Ok(shares)
    }

    /// Sends free cash to the SPV, leaving the reserve intact.
    ///
    /// NAV does not move: the cash leaves the vault and reappears as in-flight capital.
    pub fn deploy_capital(ctx: Context<SpvMovesFunds>, pool_id: u64, amount: u64) -> Result<()> {
        let _ = pool_id;
        let now = clock_now()?;
        let (total_nav, config, bump, id, spv, owed) = {
            let pool = ctx.accounts.pool.load()?;
            let queue = ctx.accounts.queue.load()?;
            (
                net_nav(&pool, &queue, ctx.accounts.vault.amount, now)?,
                piron_core::stable_yield::types::ReserveConfig {
                    min_absolute_reserve: pool.min_absolute_reserve as i128,
                    reserve_ratio_bps: pool.reserve_ratio_bps,
                },
                pool.bump,
                pool.pool_id,
                pool.spv,
                queue.total_pending_value,
            )
        };
        if ctx.accounts.spv.key() != spv {
            return Err(local(code::UNAUTHORIZED));
        }
        // Cash standing against queued exits is spoken for. Deploying it would leave the
        // pool unable to pay requests it has already priced and burned shares for.
        let free = (ctx.accounts.vault.amount as i128 - owed as i128).max(0);
        validate::validate_deployment(amount as i128, free, total_nav, &config).map_err(map_err)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_spv().with_signer(signer), amount)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        pool.in_flight = add(pool.in_flight, amount)?;
        let in_flight = pool.in_flight;
        drop(pool);
        emit!(CapitalDeployed { pool: ctx.accounts.pool.key(), amount, in_flight });
        Ok(())
    }

    /// Returns capital or yield from the SPV.
    ///
    /// Returns retire in-flight capital first; anything beyond it is yield, which shows up
    /// as a NAV gain rather than driving the outstanding balance below zero.
    pub fn return_capital(ctx: Context<SpvMovesFunds>, pool_id: u64, amount: u64) -> Result<()> {
        let _ = pool_id;
        let now = clock_now()?;
        if amount == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        let spv = ctx.accounts.pool.load()?.spv;
        if ctx.accounts.spv.key() != spv {
            return Err(local(code::UNAUTHORIZED));
        }
        token::transfer(ctx.accounts.spv_to_vault(), amount)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        let retired = amount.min(pool.in_flight);
        pool.in_flight -= retired;
        // Cash arrived and in-flight fell; anything beyond what was outstanding is yield,
        // and the recomputation picks that up without a separate increment.
        refresh_nav(&mut pool, ctx.accounts.vault.amount + amount, now)?;
        let in_flight = pool.in_flight;
        drop(pool);
        emit!(CapitalReturned { pool: ctx.accounts.pool.key(), amount, in_flight });
        Ok(())
    }

    /// Records an instrument the SPV has bought with capital already drawn.
    ///
    /// Its purchase price leaves the in-flight balance and reappears as a marked holding,
    /// so NAV does not move. Recording one larger than the capital drawn would clamp the
    /// balance at zero and leave NAV counting value the pool never had, so it is refused.
    pub fn add_instrument(
        ctx: Context<Operator>,
        pool_id: u64,
        instrument: InstrumentTerms,
    ) -> Result<u32> {
        let _ = pool_id;
        let now = clock_now()?;
        let mut pool = ctx.accounts.pool.load_mut()?;

        // Without this anyone could record a holding against capital the real SPV drew,
        // clearing its outstanding balance and claiming the attribution.
        if ctx.accounts.operator.key() != pool.spv {
            return Err(local(code::UNAUTHORIZED));
        }
        if pool.instrument_count as usize >= MAX_INSTRUMENTS {
            return Err(local(code::TOO_MANY_INSTRUMENTS));
        }
        if instrument.purchase_price == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        if instrument.purchase_price > pool.in_flight {
            return Err(local(code::EXCEEDS_IN_FLIGHT));
        }
        if instrument.maturity_date <= now as i64 {
            return Err(map_err(piron_core::Error::InvalidMaturityDate));
        }
        if instrument.face_value < instrument.purchase_price {
            return Err(local(code::INVALID_FACE_VALUE));
        }
        if instrument.instrument_type != 0 {
            if instrument.coupon_frequency == 0
                || instrument.coupon_frequency > piron_core::stable_yield::MAX_COUPON_FREQUENCY
            {
                return Err(map_err(piron_core::Error::InvalidCouponFrequency));
            }
            if instrument.annual_coupon_rate == 0
                || instrument.annual_coupon_rate > piron_core::stable_yield::MAX_ANNUAL_COUPON_RATE
            {
                return Err(local(code::INVALID_COUPON_RATE));
            }
        }

        let index = pool.instrument_count as u32;
        pool.instruments[index as usize] = StoredInstrument {
            spv: ctx.accounts.operator.key(),
            purchase_price: instrument.purchase_price,
            face_value: instrument.face_value,
            purchase_date: now as i64,
            maturity_date: instrument.maturity_date,
            next_coupon_due_date: instrument.next_coupon_due_date,
            annual_coupon_rate: instrument.annual_coupon_rate,
            coupon_frequency: instrument.coupon_frequency,
            coupons_paid: 0,
            instrument_type: instrument.instrument_type,
            is_active: 1,
            _pad: [0; 2],
        };
        pool.instrument_count += 1;
        pool.in_flight -= instrument.purchase_price;
        refresh_nav(&mut pool, ctx.accounts.vault.amount, now)?;
        drop(pool);

        emit!(InstrumentAdded { pool: ctx.accounts.pool.key(), index, purchase_price: instrument.purchase_price });
        Ok(index)
    }

    /// Settles a holding at maturity: the proceeds come back and the holding closes.
    ///
    /// A return short of face is accepted and shows up as a NAV fall — the pool took a
    /// loss, and pretending otherwise would misprice every share.
    pub fn mature_instrument(ctx: Context<SpvPaysIn>, pool_id: u64, index: u32, proceeds: u64) -> Result<()> {
        let _ = pool_id;
        let now = clock_now()?;
        if proceeds == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        let spv = ctx.accounts.pool.load()?.spv;
        if ctx.accounts.spv.key() != spv {
            return Err(local(code::UNAUTHORIZED));
        }
        token::transfer(ctx.accounts.spv_to_vault(), proceeds)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        let count = pool.instrument_count as usize;
        if index as usize >= count || pool.instruments[index as usize].is_active == 0 {
            return Err(local(code::INSTRUMENT_NOT_FOUND));
        }
        retire(&mut pool, index as usize);
        // The holding left the book and its proceeds arrived as cash. A settlement short
        // of the mark shows up as a NAV fall, which is what it is.
        refresh_nav(&mut pool, ctx.accounts.vault.amount + proceeds, now)?;
        drop(pool);

        emit!(InstrumentMatured { pool: ctx.accounts.pool.key(), index, proceeds });
        Ok(())
    }

    /// Writes a holding off the book, for a default or a sale that will not settle.
    ///
    /// A holding that is never matured keeps carrying value, and holders would redeem
    /// against an asset the pool will not receive.
    pub fn write_off_instrument(ctx: Context<Authority>, pool_id: u64, index: u32) -> Result<()> {
        let _ = pool_id;
        let mut pool = ctx.accounts.pool.load_mut()?;
        // The SPV must not be able to write off its own default.
        if ctx.accounts.authority.key() != pool.authority {
            return Err(local(code::UNAUTHORIZED));
        }
        let count = pool.instrument_count as usize;
        if index as usize >= count || pool.instruments[index as usize].is_active == 0 {
            return Err(local(code::INSTRUMENT_NOT_FOUND));
        }
        let marked = pool.instruments[index as usize].purchase_price;
        retire(&mut pool, index as usize);
        refresh_nav(&mut pool, ctx.accounts.vault.amount, clock_now()?)?;
        drop(pool);

        emit!(InstrumentWrittenOff { pool: ctx.accounts.pool.key(), index, written_off: marked });
        Ok(())
    }

    /// Joins the queue to leave. The quote is struck now, at today's price, and the shares
    /// go with it.
    pub fn request_withdrawal(ctx: Context<RequestWithdrawal>, pool_id: u64, shares: u64) -> Result<u64> {
        let _ = pool_id;
        let now = clock_now()?;
        let (nav_now, fee_bps, holding_period) = {
            let pool = ctx.accounts.pool.load()?;
            let queue = ctx.accounts.queue.load()?;
            (
                net_nav(&pool, &queue, ctx.accounts.vault.amount, now)?,
                pool.withdrawal_fee_bps,
                pool.holding_period,
            )
        };
        validate::validate_withdrawal_request(shares as i128, ctx.accounts.holder_shares.amount as i128)
            .map_err(map_err)?;

        let record = &ctx.accounts.holder_record;
        if record.owner != ctx.accounts.holder.key() {
            return Err(local(code::OWNER_MISMATCH));
        }
        if (now as i64) < record.last_deposit_at + holding_period {
            return Err(local(code::HOLDING_PERIOD));
        }

        let price = nav::nav_per_share(nav_now, ctx.accounts.share_mint.supply as i128).map_err(map_err)?;
        let quote = core_queue::quote_withdrawal(shares as i128, price, fee_bps).map_err(map_err)?;

        // Burned here, not when the queue reaches them. Left outstanding, the holder could
        // move them on and the burn at service time would fail against an emptied balance
        // — which stops the head advancing and strands every request behind it. It would
        // also let the same shares be promised twice.
        token::burn(ctx.accounts.burn_shares(), shares)?;

        let mut queue = ctx.accounts.queue.load_mut()?;
        if queue.is_full() {
            return Err(local(code::QUEUE_FULL));
        }
        let slot = queue.slot(queue.tail);
        let id = queue.tail;
        queue.requests[slot] = StoredRequest {
            owner: ctx.accounts.holder.key(),
            shares,
            request_time: now as i64,
            deadline: 0,
            estimated_value: to_u64(quote.net_value).map_err(map_err)?,
            fee_amount: to_u64(quote.fee).map_err(map_err)?,
            processed_time: 0,
            processed: 0,
            _pad: [0; 7],
        };
        queue.tail += 1;
        queue.total_pending_value = queue
            .total_pending_value
            .checked_add(to_u64(quote.gross_value).map_err(map_err)?)
            .ok_or_else(|| map_err(piron_core::Error::Overflow))?;
        drop(queue);

        emit!(WithdrawalRequested {
            pool: ctx.accounts.pool.key(),
            owner: ctx.accounts.holder.key(),
            request_id: id,
            shares,
        });
        Ok(id)
    }

    /// Settles queued exits in order, from the head, out of vault cash.
    ///
    /// The payee accounts ride in `remaining_accounts`, one per request from the head, in
    /// the same order. Each is checked against the owner recorded on its request — an
    /// operator picks the accounts, so nothing else stops it redirecting a payout.
    ///
    /// The reserve floor does not apply here. It exists so that holders can leave;
    /// withholding it from the very queue it is held for would be circular.
    pub fn process_withdrawals<'info>(
        ctx: Context<'info, ProcessWithdrawals<'info>>,
        pool_id: u64,
        max_items: u32,
    ) -> Result<u32> {
        let _ = pool_id;
        let now = clock_now()?;
        let (bump, id, treasury) = {
            let pool = ctx.accounts.pool.load()?;
            (pool.bump, pool.pool_id, pool.treasury)
        };
        if ctx.accounts.treasury_asset.owner != treasury {
            return Err(local(code::OWNER_MISMATCH));
        }

        let take = max_items.min(core_queue::MAX_WITHDRAWAL_BATCH);

        // Boxed: twenty core requests plus the cursor overrun the 4KB frame a Solana
        // instruction gets, and the program heap has room to spare.
        let mut buf = Box::new([EMPTY_REQUEST; core_queue::MAX_WITHDRAWAL_BATCH as usize]);
        let mut count = 0usize;
        let cursor = {
            let queue = ctx.accounts.queue.load()?;
            if queue.pool != ctx.accounts.pool.key() {
                return Err(local(code::WRONG_QUEUE));
            }
            for offset in 0..take as u64 {
                if queue.head + offset >= queue.tail {
                    break;
                }
                buf[count] = queue.requests[queue.slot(queue.head + offset)].to_core();
                count += 1;
            }
            piron_core::stable_yield::types::WithdrawalQueue {
                head: queue.head,
                tail: queue.tail,
                total_pending_value: 0,
            }
        };

        let outcome =
            core_queue::process_batch(&cursor, &buf[..count], ctx.accounts.vault.amount as i128, take)
                .map_err(map_err)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        let mut moved: u64 = 0;

        // Driven by queue position, not by a count of payments. `new_head` also steps
        // over entries that were already processed, which `processed` does not count — so
        // indexing payees by payment number would shift them by one the moment the head
        // rested on a served entry, paying the wrong account and skipping a request while
        // advancing past it.
        let mut payee_index = 0usize;
        for position in cursor.head..outcome.new_head {
            let (slot, owner, net, fee, already) = {
                let queue = ctx.accounts.queue.load()?;
                let slot = queue.slot(position);
                let r = &queue.requests[slot];
                (slot, r.owner, r.estimated_value, r.fee_amount, r.processed != 0)
            };
            if already {
                continue;
            }
            let i = payee_index;
            payee_index += 1;

            // The operator chooses these accounts, so nothing but this stops it sending a
            // holder's money somewhere else.
            let payee = ctx
                .remaining_accounts
                .get(i)
                .ok_or_else(|| local(code::OWNER_MISMATCH))?;
            if Account::<TokenAccount>::try_from(payee)?.owner != owner {
                return Err(local(code::OWNER_MISMATCH));
            }

            token::transfer(ctx.accounts.vault_to(payee).with_signer(signer), net)?;
            if fee > 0 {
                token::transfer(ctx.accounts.vault_to_treasury().with_signer(signer), fee)?;
            }
            moved = moved
                .checked_add(net)
                .and_then(|m| m.checked_add(fee))
                .ok_or_else(|| map_err(piron_core::Error::Overflow))?;

            let mut queue = ctx.accounts.queue.load_mut()?;
            queue.requests[slot].processed = 1;
            queue.requests[slot].processed_time = now as i64;
        }

        {
            let mut queue = ctx.accounts.queue.load_mut()?;
            queue.head = outcome.new_head;
            queue.total_pending_value = queue.total_pending_value.saturating_sub(moved);
        }
        {
            // Both halves left the vault, and pending fell by the same gross amount,
            // which is why the price does not move.
            let mut pool = ctx.accounts.pool.load_mut()?;
            let left = ctx.accounts.vault.amount.saturating_sub(moved);
            refresh_nav(&mut pool, left, now)?;
        }

        emit!(WithdrawalsProcessed {
            pool: ctx.accounts.pool.key(),
            processed: outcome.processed,
            total_paid: to_u64(outcome.total_paid).map_err(map_err)?,
            new_head: outcome.new_head,
            stopped_for_liquidity: outcome.stopped_for_liquidity,
        });
        Ok(outcome.processed)
    }

    /// Leaves the pool in one step, when the vault can cover it and no one is waiting.
    ///
    /// Paying straight out while a queue stands would let a holder step in front of people
    /// who asked first, so this is refused whenever the queue has anything in it.
    pub fn withdraw(ctx: Context<Withdraw>, pool_id: u64, shares: u64) -> Result<u64> {
        let _ = pool_id;
        let now = clock_now()?;
        let (nav_now, fee_bps, holding_period, bump, id, treasury) = {
            let pool = ctx.accounts.pool.load()?;
            let queue = ctx.accounts.queue.load()?;
            if !queue.is_empty() {
                return Err(local(code::QUEUE_NOT_EMPTY));
            }
            (
                net_nav(&pool, &queue, ctx.accounts.vault.amount, now)?,
                pool.withdrawal_fee_bps,
                pool.holding_period,
                pool.bump,
                pool.pool_id,
                pool.treasury,
            )
        };
        if ctx.accounts.treasury_asset.owner != treasury {
            return Err(local(code::OWNER_MISMATCH));
        }
        validate::validate_withdrawal_request(shares as i128, ctx.accounts.holder_shares.amount as i128)
            .map_err(map_err)?;

        let record = &ctx.accounts.holder_record;
        if record.owner != ctx.accounts.holder.key() {
            return Err(local(code::OWNER_MISMATCH));
        }
        if (now as i64) < record.last_deposit_at + holding_period {
            return Err(local(code::HOLDING_PERIOD));
        }

        let price = nav::nav_per_share(nav_now, ctx.accounts.share_mint.supply as i128).map_err(map_err)?;
        let quote = core_queue::quote_withdrawal(shares as i128, price, fee_bps).map_err(map_err)?;
        let gross = to_u64(quote.gross_value).map_err(map_err)?;
        let net = to_u64(quote.net_value).map_err(map_err)?;
        let fee = to_u64(quote.fee).map_err(map_err)?;

        // Nothing to fall back on: without the cash on hand the holder must queue.
        if gross > ctx.accounts.vault.amount {
            return Err(map_err(piron_core::Error::InsufficientLiquidity));
        }

        token::burn(ctx.accounts.burn_shares(), shares)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_holder().with_signer(signer), net)?;
        if fee > 0 {
            token::transfer(ctx.accounts.vault_to_treasury().with_signer(signer), fee)?;
        }

        let mut pool = ctx.accounts.pool.load_mut()?;
        refresh_nav(&mut pool, ctx.accounts.vault.amount.saturating_sub(gross), now)?;
        drop(pool);

        emit!(Withdrawn {
            pool: ctx.accounts.pool.key(),
            owner: ctx.accounts.holder.key(),
            shares,
            assets: net,
        });
        Ok(net)
    }

    /// Opens a holding record for an address that received shares by transfer.
    ///
    /// Shares are an SPL token and SPL has no transfer hook, so the program cannot see a
    /// transfer happen. Without this a transferee has no record, the withdrawal
    /// constraint finds no account, and their shares are unredeemable — money the
    /// protocol has no way to release.
    ///
    /// The clock starts here rather than at zero, so arriving by transfer is not a way
    /// around the holding period. It is not a complete binding of the period to the
    /// shares themselves: an address could register early and receive shares later.
    /// Closing that needs a Token-2022 transfer hook, which is a deployment decision
    /// rather than a change to this program.
    pub fn register_holder(ctx: Context<RegisterHolder>, pool_id: u64) -> Result<()> {
        let _ = pool_id;
        let now = clock_now()?;
        let record = &mut ctx.accounts.holder_record;
        if record.owner != Pubkey::default() {
            return Err(local(code::ALREADY_REGISTERED));
        }
        record.pool = ctx.accounts.pool.key();
        record.owner = ctx.accounts.holder.key();
        record.last_deposit_at = now as i64;
        record.bump = ctx.bumps.holder_record;
        emit!(HolderRegistered { pool: ctx.accounts.pool.key(), holder: record.owner });
        Ok(())
    }

    /// Re-stamps the stored NAV mirror at the current time.
    ///
    /// Permissionless, and deliberately so: it only writes what anyone can already
    /// compute from the account. Instructions price against a fresh computation
    /// regardless, so this exists for readers — Solana has no view functions, and
    /// between mutations the stored figure would otherwise lag the accrual.
    pub fn refresh_nav_mirror(ctx: Context<RefreshNav>, pool_id: u64) -> Result<()> {
        let _ = pool_id;
        let now = clock_now()?;
        let vault = ctx.accounts.vault.amount;
        let mut pool = ctx.accounts.pool.load_mut()?;
        refresh_nav(&mut pool, vault, now)?;
        let total_nav = pool.total_nav;
        drop(pool);
        emit!(NavRefreshed { pool: ctx.accounts.pool.key(), total_nav });
        Ok(())
    }

    /// Books a coupon the SPV has collected on a holding.
    ///
    /// The cash arrives in the vault and NAV rises by it. The instrument's own mark already
    /// accrues the coupon between payment dates, so the counter has to move with the money
    /// — otherwise the same coupon is marked and banked, and NAV counts it twice.
    pub fn record_instrument_coupon(
        ctx: Context<SpvPaysIn>,
        pool_id: u64,
        index: u32,
        amount: u64,
    ) -> Result<()> {
        let _ = pool_id;
        if amount == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        let spv = ctx.accounts.pool.load()?.spv;
        if ctx.accounts.spv.key() != spv {
            return Err(local(code::UNAUTHORIZED));
        }
        {
            let pool = ctx.accounts.pool.load()?;
            let held = &pool.instruments[index as usize];
            if index as usize >= pool.instrument_count as usize || held.is_active == 0 {
                return Err(local(code::INSTRUMENT_NOT_FOUND));
            }
            if held.coupon_frequency == 0 {
                return Err(map_err(piron_core::Error::InvalidCouponFrequency));
            }
        }
        token::transfer(ctx.accounts.spv_to_vault(), amount)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        let period = nav::SECONDS_PER_YEAR / pool.instruments[index as usize].coupon_frequency.max(1) as u64;
        let held = &mut pool.instruments[index as usize];
        held.coupons_paid += 1;
        held.next_coupon_due_date = held.next_coupon_due_date.saturating_add(period as i64);
        // The accrual the mark was carrying has become cash. Recomputing nets the two
        // rather than adding the coupon on top of an accrual already counted.
        let now = clock_now()?;
        refresh_nav(&mut pool, ctx.accounts.vault.amount + amount, now)?;
        drop(pool);

        emit!(CouponRecorded { pool: ctx.accounts.pool.key(), index, amount });
        Ok(())
    }
}

/// A slot the batch has not loaded. `processed` so a short batch steps over the tail
/// rather than trying to pay an empty request.
const EMPTY_REQUEST: piron_core::stable_yield::types::WithdrawalRequest =
    piron_core::stable_yield::types::WithdrawalRequest {
        shares: 0,
        request_time: 0,
        deadline: 0,
        estimated_value: 0,
        fee_amount: 0,
        processed: true,
        processed_time: 0,
    };

/// Drops a settled holding out of the book by swap-removal.
///
/// Marking it inactive and leaving it in place would be simpler, but the purchase cap
/// counts entries rather than live ones: a pool that never closes would reach the cap on
/// its 33rd purchase ever, with every earlier holding long since settled.
/// What the pool is worth to the holders still in it.
///
/// Queued exits burned their shares the moment they asked to leave, so the cash owed to
/// them belongs to no current share. Pricing against the gross assets would hand the
/// leavers' money to whoever remained, and charge the next depositor for it.
fn net_nav(
    pool: &StableYieldPool,
    queue: &WithdrawalQueue,
    vault_amount: u64,
    now: u64,
) -> Result<i128> {
    if queue.pool != pool_key_of(pool) {
        return Err(local(code::WRONG_QUEUE));
    }
    Ok(gross_nav(pool, vault_amount, now)? - queue.total_pending_value as i128)
}

/// What the pool holds, marked to model.
///
/// Three lines: instruments at today's value, cash in the vault, and capital in flight
/// with the SPV. Computed on every read rather than carried as a running total — a
/// counter only moves when somebody calls something, so a pool holding a bill that
/// accretes daily would price flat for the whole term and then jump at settlement,
/// handing the term's yield to whoever happened to be holding at that moment.
fn gross_nav(pool: &StableYieldPool, vault_amount: u64, now: u64) -> Result<i128> {
    let mut total = vault_amount as i128 + pool.in_flight as i128;
    for held in pool.instruments[..pool.instrument_count as usize].iter() {
        if held.is_active != 0 {
            // One at a time: thirty-two core holdings at once would not fit the 4KB
            // frame a Solana instruction gets.
            let value = nav::instrument_value(&held.to_core(), now).map_err(map_err)?;
            total = total
                .checked_add(value)
                .ok_or_else(|| map_err(piron_core::Error::Overflow))?;
        }
    }
    Ok(total)
}

/// Refreshes the stored mirror of NAV.
///
/// Solana has no view functions, so indexers read the account. The field is written from
/// the same computation the program prices against, so the two cannot drift.
fn refresh_nav(pool: &mut StableYieldPool, vault_amount: u64, now: u64) -> Result<()> {
    pool.total_nav = to_u64(gross_nav(pool, vault_amount, now)?).map_err(map_err)?;
    Ok(())
}

fn pool_key_of(pool: &StableYieldPool) -> Pubkey {
    seeds::pool(pool.pool_id).0
}

fn retire(pool: &mut StableYieldPool, index: usize) {
    let last = pool.instrument_count as usize - 1;
    if index != last {
        pool.instruments[index] = pool.instruments[last];
    }
    pool.instruments[last] = StoredInstrument::default();
    pool.instrument_count -= 1;
}

/// Terms handed in when a purchase is recorded.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InstrumentTerms {
    /// 0 discounted, 1 interest-bearing.
    pub instrument_type: u8,
    pub purchase_price: u64,
    pub face_value: u64,
    pub maturity_date: i64,
    pub annual_coupon_rate: u32,
    pub coupon_frequency: u32,
    pub next_coupon_due_date: i64,
}
// ---------------------------------------------------------------- contexts
//
// The pool and the queue are zero-copy, so they arrive as `AccountLoader` and are opened
// with `load()` / `load_mut()` inside the handler. That also means a seed constraint
// cannot read `pool.pool_id` in the attribute — nothing is deserialised yet — so pool_id
// comes in as an instruction argument and the seeds are checked against it.

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct InitializePool<'info> {
    #[account(init, payer = authority, space = StableYieldPool::LEN,
              seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    pub asset_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = authority, seeds = [seeds::VAULT, pool.key().as_ref()], bump,
              token::mint = asset_mint, token::authority = pool)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = authority, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump,
              mint::decimals = asset_mint.decimals, mint::authority = pool)]
    pub share_mint: Box<Account<'info, Mint>>,
    /// A client-created keypair account, not a PDA.
    ///
    /// This is the one place the design bends to the runtime. A CPI can allocate at most
    /// 10,240 bytes and the queue needs 22,600, and a PDA cannot sign a top-level create,
    /// so it cannot be a PDA and be this large. The client creates it as an ordinary
    /// account owned by this program and the pool records its address; anything deriving
    /// the queue reads `pool.queue` rather than computing a seed.
    #[account(zero)]
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct Deposit<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    /// Read for pricing: shares are struck against assets net of what the pool already
    /// owes queued leavers.
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(init_if_needed, payer = holder, space = HolderRecord::LEN,
              seeds = [seeds::HOLDER, pool.key().as_ref(), holder.key().as_ref()], bump)]
    pub holder_record: Box<Account<'info, HolderRecord>>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

impl<'info> Deposit<'info> {
    fn holder_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.holder_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.holder.to_account_info(),
        })
    }
    fn mint_shares(&self) -> CpiContext<'_, '_, '_, 'info, MintTo<'info>> {
        CpiContext::new(self.token_program.key(), MintTo {
            mint: self.share_mint.to_account_info(),
            to: self.holder_shares.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct SpvMovesFunds<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SpvMovesFunds<'info> {
    fn vault_to_spv(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.spv_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
    fn spv_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.spv_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.spv.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct Authority<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    /// Read only, for the NAV mirror.
    #[account(seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub authority: Signer<'info>,
}

/// The SPV records what it bought, so the instrument carries its key and per-SPV
/// attribution survives without an allocation record.
#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct Operator<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    /// Read only, for the NAV mirror.
    #[account(seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub operator: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct SpvPaysIn<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SpvPaysIn<'info> {
    fn spv_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.spv_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.spv.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct RequestWithdrawal<'info> {
    #[account(seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(mut)]
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    /// Read only: the quote is struck against marked value plus cash on hand.
    #[account(seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, Mint>>,
    /// Read, not trusted: the quote is struck against the balance the holder actually has.
    #[account(mut, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(seeds = [seeds::HOLDER, pool.key().as_ref(), holder.key().as_ref()], bump = holder_record.bump)]
    pub holder_record: Box<Account<'info, HolderRecord>>,
    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> RequestWithdrawal<'info> {
    fn burn_shares(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        CpiContext::new(self.token_program.key(), Burn {
            mint: self.share_mint.to_account_info(),
            from: self.holder_shares.to_account_info(),
            authority: self.holder.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct RegisterHolder<'info> {
    #[account(seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(init_if_needed, payer = holder, space = HolderRecord::LEN,
              seeds = [seeds::HOLDER, pool.key().as_ref(), holder.key().as_ref()], bump)]
    pub holder_record: Box<Account<'info, HolderRecord>>,
    #[account(mut)]
    pub holder: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct RefreshNav<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct ProcessWithdrawals<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(mut)]
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_asset: Box<Account<'info, TokenAccount>>,
    pub operator: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> ProcessWithdrawals<'info> {
    fn vault_to(&self, payee: &AccountInfo<'info>) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: payee.clone(),
            authority: self.pool.to_account_info(),
        })
    }
    fn vault_to_treasury(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.treasury_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct Withdraw<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(seeds = [seeds::HOLDER, pool.key().as_ref(), holder.key().as_ref()], bump = holder_record.bump)]
    pub holder_record: Box<Account<'info, HolderRecord>>,
    #[account(mut)]
    pub treasury_asset: Box<Account<'info, TokenAccount>>,
    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Withdraw<'info> {
    fn burn_shares(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        CpiContext::new(self.token_program.key(), Burn {
            mint: self.share_mint.to_account_info(),
            from: self.holder_shares.to_account_info(),
            authority: self.holder.to_account_info(),
        })
    }
    fn vault_to_holder(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.holder_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
    fn vault_to_treasury(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.treasury_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
}

// ---------------------------------------------------------------- events

#[event] pub struct PoolInitialized { pub pool: Pubkey, pub pool_id: u64 }

#[event] pub struct Deposited { pub pool: Pubkey, pub investor: Pubkey, pub assets: u64, pub shares: u64 }
#[event] pub struct CapitalDeployed { pub pool: Pubkey, pub amount: u64, pub in_flight: u64 }
#[event] pub struct CapitalReturned { pub pool: Pubkey, pub amount: u64, pub in_flight: u64 }
#[event] pub struct InstrumentAdded { pub pool: Pubkey, pub index: u32, pub purchase_price: u64 }
#[event] pub struct InstrumentMatured { pub pool: Pubkey, pub index: u32, pub proceeds: u64 }
#[event] pub struct InstrumentWrittenOff { pub pool: Pubkey, pub index: u32, pub written_off: u64 }
#[event] pub struct WithdrawalRequested { pub pool: Pubkey, pub owner: Pubkey, pub request_id: u64, pub shares: u64 }
#[event] pub struct WithdrawalsProcessed { pub pool: Pubkey, pub processed: u32, pub total_paid: u64, pub new_head: u64, pub stopped_for_liquidity: bool }
#[event] pub struct Withdrawn { pub pool: Pubkey, pub owner: Pubkey, pub shares: u64, pub assets: u64 }
#[event] pub struct HolderRegistered { pub pool: Pubkey, pub holder: Pubkey }
#[event] pub struct NavRefreshed { pub pool: Pubkey, pub total_nav: u64 }
#[event] pub struct CouponRecorded { pub pool: Pubkey, pub index: u32, pub amount: u64 }