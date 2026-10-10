//! Single-Asset pool — Solana binding layer.
//!
//! The same entry points as the Soroban binding, in the same order, so the two can be read
//! side by side. Every protocol decision is delegated to `piron-core`; nothing here decides
//! anything.
//!
//! What differs from the Soroban binding, and why:
//!
//! - **No TTL, no keeper.** A rent-exempt account lives indefinitely. There is no
//!   `bump_instance` equivalent and there should not be one.
//! - **Accounts are declared, not discovered.** Anchor's `Accounts` context is the
//!   transaction's footprint: every account touched is named up front and its PDA
//!   derivation is checked by the framework.
//! - **Signers are checked, not asserted.** `Signer<'info>` is the analogue of
//!   `require_auth()`, except Soroban's host verifies signatures for you and here the
//!   framework does it at deserialization.
//! - **Money is `u64`.** Widening into core is free; narrowing back goes through
//!   [`state::to_u64`], which refuses rather than truncates.
//! - **There are no view functions.** Soroban's `pool_value` and `status` are calls; here a
//!   client fetches the account and reads it. So anything a caller needs to see has to be
//!   *stored*, which is why `pool_value` is a field written on every mutation rather than a
//!   function. A client that recomputed it would eventually disagree with what the program
//!   believed when it moved money.
//! - **The pool holds nothing.** A program cannot own tokens, so the asset sits in a vault
//!   token account and the pool PDA is its authority. That PDA is also the share mint's
//!   authority, which is what makes minting a signed CPI rather than an internal call.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};

extern crate alloc;

pub mod seeds;
pub mod state;

use piron_core::single_asset::{calc, flow, lifecycle, validate};
use piron_core::types::PoolStatus;
use state::{to_u64, Pool, Position};

declare_id!("ZmF2NjKuiRrE8NYG7Kou1BzBPRV6VYRsXsUCqLpjBmR");

/// Maps a core error onto a raw custom program error.
///
/// Deliberately **not** an Anchor `#[error_code]` enum: Anchor offsets custom errors by
/// 6000, which would make the same protocol condition report a different number on Solana
/// than on Stellar. Returning `ProgramError::Custom` with the core code verbatim keeps one
/// taxonomy across all three runtimes, which is what the backend maps on.
fn map_err(err: piron_core::Error) -> Error {
    Error::from(ProgramError::Custom(err.code()))
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| map_err(piron_core::Error::Overflow))
}

#[program]
pub mod piron_single_asset {
    use super::*;

    /// Creates the pool, its vault and its share mint in one transaction.
    ///
    /// Soroban's factory deploys a contract per pool. Here one program serves every pool
    /// and the pool is a PDA, so "deploying a pool" is initialising accounts.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_pool(ctx: Context<InitializePool>, pool_id: u64, terms: PoolTerms) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.pool_id = pool_id;
        pool.authority = ctx.accounts.authority.key();
        pool.spv = terms.spv;
        pool.treasury = terms.treasury;
        pool.asset_mint = ctx.accounts.asset_mint.key();
        pool.vault = ctx.accounts.vault.key();
        pool.share_mint = ctx.accounts.share_mint.key();

        pool.instrument_type = terms.instrument_type;
        pool.face_value = 0;
        pool.purchase_price = terms.purchase_price;
        pool.target_raise = terms.target_raise;
        pool.epoch_end_time = terms.epoch_end_time;
        pool.maturity_date = terms.maturity_date;
        pool.coupon_count = terms.coupon_count;
        pool.coupon_dates = terms.coupon_dates;
        pool.coupon_rates = terms.coupon_rates;
        pool.discount_rate = terms.discount_rate;
        pool.minimum_funding_threshold = terms.minimum_funding_threshold;
        pool.min_investment = terms.min_investment;
        pool.withdrawal_fee_bps = terms.withdrawal_fee_bps;

        pool.status = PoolStatus::Funding as u8;
        pool.bump = ctx.bumps.pool;

        // Refuse a pool that cannot reach a valid settlement, rather than letting it fail
        // at epoch close with deposits already locked in.
        let data = pool.to_core().map_err(map_err)?;
        validate::validate_config(&data.config).map_err(map_err)?;

        pool.pool_value = 0;
        emit!(PoolInitialized { pool: pool.key(), pool_id, asset_mint: pool.asset_mint });
        Ok(())
    }

    /// Buys into the pool during the funding window. Shares are struck 1:1.
    pub fn deposit(ctx: Context<Deposit>, assets: u64) -> Result<u64> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let outcome = flow::deposit(&data, assets as i128, now, ctx.accounts.position.deposit_time as u64)
            .map_err(map_err)?;

        token::transfer(ctx.accounts.into_deposit(), assets)?;

        let shares = to_u64(outcome.shares).map_err(map_err)?;
        let pool_key = ctx.accounts.pool.key();
        let bump = ctx.accounts.pool.bump;
        let pool_id = ctx.accounts.pool.pool_id;
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::mint_to(ctx.accounts.into_mint_shares().with_signer(signer), shares)?;

        let pool = &mut ctx.accounts.pool;
        pool.total_raised = to_u64(outcome.total_raised).map_err(map_err)?;
        pool.refresh_value(now)?;

        let position = &mut ctx.accounts.position;
        if outcome.starts_position {
            position.pool = pool_key;
            position.owner = ctx.accounts.investor.key();
            position.deposit_time = now as i64;
            position.bump = ctx.bumps.position;
        }
        position.shares = add(position.shares, shares)?;

        emit!(DepositRecorded {
            pool: pool_key,
            investor: ctx.accounts.investor.key(),
            assets,
            shares,
            total_raised: pool.total_raised,
        });
        Ok(shares)
    }

    /// Marks the pool filled once the target raise is met. Permissionless: it only
    /// observes a fact anyone can verify.
    pub fn mark_filled(ctx: Context<PoolOnly>) -> Result<()> {
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let status = lifecycle::mark_filled(&data).map_err(map_err)?;
        ctx.accounts.pool.set_status(status);
        Ok(())
    }

    /// Closes the funding epoch: on to investment, or to emergency if underfunded.
    pub fn close_epoch(ctx: Context<Authority>, force: bool) -> Result<()> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let close = lifecycle::close_epoch(&data, now, force).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.face_value = to_u64(close.face_value).map_err(map_err)?;
        pool.set_status(close.new_status);
        pool.refresh_value(now)?;

        emit!(EpochClosed {
            pool: pool.key(),
            status: pool.status,
            underfunded: close.underfunded,
        });
        Ok(())
    }

    /// Releases funds to the SPV to invest.
    pub fn withdraw_for_investment(ctx: Context<SpvMovesFunds>, amount: u64) -> Result<()> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        lifecycle::validate_spv_withdrawal(&data, amount as i128).map_err(map_err)?;

        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_spv().with_signer(signer), amount)?;

        let pool = &mut ctx.accounts.pool;
        pool.funds_withdrawn_by_spv = add(pool.funds_withdrawn_by_spv, amount)?;
        pool.refresh_value(now)?;

        emit!(FundsWithdrawnBySpv { pool: pool.key(), amount });
        Ok(())
    }

    /// Confirms what the SPV deployed, fixing face value and the discount to accrue.
    pub fn confirm_investment(ctx: Context<Authority>, actual_amount: u64, proof: String) -> Result<()> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let outcome = lifecycle::process_investment(&data, actual_amount as i128, now).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.actual_invested = to_u64(outcome.actual_invested).map_err(map_err)?;
        pool.face_value = to_u64(outcome.face_value).map_err(map_err)?;
        pool.total_discount_earned = to_u64(outcome.total_discount_earned).map_err(map_err)?;
        pool.set_status(outcome.new_status);
        pool.refresh_value(now)?;

        emit!(InvestmentConfirmed { pool: pool.key(), actual_amount, proof });
        Ok(())
    }

    /// Records a coupon the SPV has paid in.
    pub fn record_coupon(ctx: Context<SpvPaysIn>, amount: u64) -> Result<()> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        validate::validate_coupon_payment(&data, amount as i128).map_err(map_err)?;

        token::transfer(ctx.accounts.spv_to_vault(), amount)?;

        let pool = &mut ctx.accounts.pool;
        pool.total_coupons_received = add(pool.total_coupons_received, amount)?;
        pool.refresh_value(now)?;

        emit!(CouponReceived {
            pool: pool.key(),
            amount,
            total_received: pool.total_coupons_received,
        });
        Ok(())
    }

    /// Makes received coupons claimable by holders.
    pub fn distribute_coupons(ctx: Context<Authority>) -> Result<u64> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let undistributed = calc::undistributed_coupons(&data);
        if undistributed <= 0 {
            return Err(map_err(piron_core::Error::NoCouponsToDistribute));
        }

        let pool = &mut ctx.accounts.pool;
        pool.total_coupons_distributed = add(pool.total_coupons_distributed, to_u64(undistributed).map_err(map_err)?)?;

        emit!(CouponsDistributed { pool: pool.key(), amount: pool.total_coupons_distributed });
        Ok(pool.total_coupons_distributed)
    }

    /// Claims a holder's share of distributed coupons.
    ///
    /// Shares are read from the holder's token account rather than taken as an argument.
    /// The scaffold trusted a caller-supplied figure, which would have let anyone claim
    /// against a balance they did not hold.
    pub fn claim_coupon(ctx: Context<ClaimCoupon>) -> Result<u64> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        // Subscribed shares, not the live token balance — see `Position::shares`.
        let user_shares = ctx.accounts.position.shares as i128;
        let total_shares = ctx.accounts.share_mint.supply as i128;

        let claim = calc::claim_coupon(
            &data,
            user_shares,
            total_shares,
            ctx.accounts.position.coupons_claimed as i128,
        )
        .map_err(map_err)?;

        let claimable = to_u64(claim.claimable).map_err(map_err)?;
        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_holder().with_signer(signer), claimable)?;

        ctx.accounts.position.coupons_claimed = to_u64(claim.new_claimed_total).map_err(map_err)?;
        let pool = &mut ctx.accounts.pool;
        pool.total_coupons_claimed = add(pool.total_coupons_claimed, claimable)?;
        pool.refresh_value(now)?;

        emit!(CouponClaimed {
            pool: pool.key(),
            holder: ctx.accounts.holder.key(),
            claimable,
        });
        Ok(claimable)
    }

    /// Settles the deal with what the SPV returned, freezing the redemption divisor.
    pub fn settle_maturity(ctx: Context<SpvPaysIn>, final_amount: u64) -> Result<()> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let total_shares = ctx.accounts.share_mint.supply as i128;
        let outcome = lifecycle::process_maturity(&data, final_amount as i128, now, total_shares)
            .map_err(map_err)?;

        token::transfer(ctx.accounts.spv_to_vault(), final_amount)?;

        let pool = &mut ctx.accounts.pool;
        pool.funds_returned_by_spv = to_u64(outcome.funds_returned).map_err(map_err)?;
        // Frozen here, and divided by at every redemption. Live supply shrinks as holders
        // exit, which would hand each successive holder a larger slice of the same money.
        pool.shares_at_maturity = to_u64(outcome.shares_at_maturity).map_err(map_err)?;
        pool.set_status(outcome.new_status);
        pool.refresh_value(now)?;

        emit!(MaturitySettled {
            pool: pool.key(),
            final_amount,
            shares_at_maturity: pool.shares_at_maturity,
        });
        Ok(())
    }

    /// Redeems a holder's position. Which path applies depends on the pool's status.
    pub fn withdraw(ctx: Context<Withdraw>, assets: u64) -> Result<u64> {
        let now = clock_now()?;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;

        let outcome = flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: assets as i128,
                user_shares: ctx.accounts.holder_shares.amount as i128,
                total_shares: ctx.accounts.share_mint.supply as i128,
                now,
            },
        )
        .map_err(map_err)?;

        let burned = to_u64(outcome.shares_burned).map_err(map_err)?;
        let net = to_u64(outcome.net).map_err(map_err)?;
        let fee = to_u64(outcome.fee).map_err(map_err)?;

        token::burn(ctx.accounts.burn_shares(), burned)?;

        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_holder().with_signer(signer), net)?;
        if fee > 0 {
            token::transfer(ctx.accounts.vault_to_treasury().with_signer(signer), fee)?;
        }

        let pool = &mut ctx.accounts.pool;
        pool.total_raised = to_u64(outcome.total_raised).map_err(map_err)?;
        pool.total_fees_collected = add(pool.total_fees_collected, fee)?;
        pool.refresh_value(now)?;

        // Subscribed shares fall with the burn, so the entitlement basis and the supply
        // stay in step.
        ctx.accounts.position.shares = ctx.accounts.position.shares.saturating_sub(burned);
        if outcome.clears_position {
            ctx.accounts.position.deposit_time = 0;
        }

        emit!(PoolWithdrawal {
            pool: pool.key(),
            owner: ctx.accounts.holder.key(),
            assets: net,
            shares: burned,
            kind: outcome.kind as u8,
        });
        Ok(burned)
    }

    /// Closes a fully-redeemed pool.
    pub fn mark_withdrawn(ctx: Context<AuthorityWithMint>) -> Result<()> {
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let status = lifecycle::mark_withdrawn(&data, ctx.accounts.share_mint.supply as i128)
            .map_err(map_err)?;
        ctx.accounts.pool.set_status(status);
        Ok(())
    }

    /// Halts the pool and opens refunds.
    ///
    /// Only while the escrow is still whole. The refund pays one unit per share, which is
    /// right when nothing has been invested and wrong the moment it has: against a vault
    /// already drawn down, the first claimants take par and the rest find nothing.
    pub fn declare_emergency(ctx: Context<Authority>, trigger: String) -> Result<()> {
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let status = lifecycle::declare_emergency(&data).map_err(map_err)?;
        let pool = &mut ctx.accounts.pool;
        pool.set_status(status);
        emit!(EmergencyDeclared { pool: pool.key(), trigger });
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

fn clock_now() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}

// The pool PDA is the authority over the vault and the share mint, so every CPI that
// moves either has to present its seeds. They are built inline at each call site rather
// than in a helper: the slice borrows the little-endian pool id, which must outlive the
// call, and a function returning it would borrow a temporary.

/// Terms handed in at creation. A struct rather than fifteen arguments, because Solana
/// instruction data is borsh and the argument list is the ABI.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PoolTerms {
    pub spv: Pubkey,
    pub treasury: Pubkey,
    pub instrument_type: u8,
    pub purchase_price: u64,
    pub target_raise: u64,
    pub epoch_end_time: i64,
    pub maturity_date: i64,
    pub coupon_count: u8,
    pub coupon_dates: [i64; piron_core::MAX_COUPON_SCHEDULE],
    pub coupon_rates: [u32; piron_core::MAX_COUPON_SCHEDULE],
    pub discount_rate: u32,
    pub minimum_funding_threshold: u32,
    pub min_investment: u64,
    pub withdrawal_fee_bps: u32,
}

// ---------------------------------------------------------------- events
//
// Solana has no indexed logs. `emit!` writes base64 into the transaction's program logs,
// and an indexer walks signatures for the program address and parses them. So the pool key
// travels *in* the event rather than as a filterable topic, and the backend's cursor is a
// signature rather than a block number.

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub pool_id: u64,
    pub asset_mint: Pubkey,
}

#[event]
pub struct DepositRecorded {
    pub pool: Pubkey,
    pub investor: Pubkey,
    pub assets: u64,
    pub shares: u64,
    pub total_raised: u64,
}

#[event]
pub struct EpochClosed {
    pub pool: Pubkey,
    pub status: u8,
    pub underfunded: bool,
}

#[event]
pub struct FundsWithdrawnBySpv {
    pub pool: Pubkey,
    pub amount: u64,
}

#[event]
pub struct InvestmentConfirmed {
    pub pool: Pubkey,
    pub actual_amount: u64,
    pub proof: String,
}

#[event]
pub struct CouponReceived {
    pub pool: Pubkey,
    pub amount: u64,
    pub total_received: u64,
}

#[event]
pub struct CouponsDistributed {
    pub pool: Pubkey,
    pub amount: u64,
}

#[event]
pub struct CouponClaimed {
    pub pool: Pubkey,
    pub holder: Pubkey,
    pub claimable: u64,
}

#[event]
pub struct MaturitySettled {
    pub pool: Pubkey,
    pub final_amount: u64,
    pub shares_at_maturity: u64,
}

/// The event the indexer counts positions from. `kind` mirrors the Solidity:
/// 0 funding-phase refund, 1 matured redemption, 2 emergency exit.
#[event]
pub struct PoolWithdrawal {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub assets: u64,
    pub shares: u64,
    pub kind: u8,
}

#[event]
pub struct EmergencyDeclared {
    pub pool: Pubkey,
    pub trigger: String,
}

// ---------------------------------------------------------------- contexts
//
// Every account an instruction touches is named here, and Anchor checks each PDA against
// the seeds before the handler runs. This is the transaction's footprint: the backend has
// to assemble exactly this list to build a transaction, which is why `seeds.rs` is shared
// rather than re-spelled on the client.

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct InitializePool<'info> {
    #[account(
        init,
        payer = authority,
        space = Pool::LEN,
        seeds = [seeds::POOL, &pool_id.to_le_bytes()],
        bump
    )]
    pub pool: Account<'info, Pool>,

    pub asset_mint: Box<Account<'info, Mint>>,

    /// The vault is the Soroban escrow. A program cannot hold tokens, so it holds the
    /// authority over an account that does.
    #[account(
        init,
        payer = authority,
        seeds = [seeds::VAULT, pool.key().as_ref()],
        bump,
        token::mint = asset_mint,
        token::authority = pool,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// Shares. A plain SPL mint, decimals matched to the asset so one share is one unit
    /// during funding, and the pool is the only thing that can mint or burn.
    #[account(
        init,
        payer = authority,
        seeds = [seeds::SHARE_MINT, pool.key().as_ref()],
        bump,
        mint::decimals = asset_mint.decimals,
        mint::authority = pool,
    )]
    pub share_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    #[account(
        init_if_needed,
        payer = investor,
        space = Position::LEN,
        seeds = [seeds::POSITION, pool.key().as_ref(), investor.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = pool.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,

    #[account(mut, token::mint = pool.asset_mint, token::authority = investor)]
    pub investor_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.share_mint, token::authority = investor)]
    pub investor_shares: Box<Account<'info, TokenAccount>>,

    /// The analogue of `investor.require_auth()` on Soroban.
    #[account(mut)]
    pub investor: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

impl<'info> Deposit<'info> {
    fn into_deposit(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.investor_asset.to_account_info(),
                to: self.vault.to_account_info(),
                authority: self.investor.to_account_info(),
            },
        )
    }

    fn into_mint_shares(&self) -> CpiContext<'_, '_, '_, 'info, MintTo<'info>> {
        CpiContext::new(
            self.token_program.key(),
            MintTo {
                mint: self.share_mint.to_account_info(),
                to: self.investor_shares.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct PoolOnly<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,
}

#[derive(Accounts)]
pub struct Authority<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,
    #[account(address = pool.authority)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct AuthorityWithMint<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,
    #[account(address = pool.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(address = pool.authority)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct SpvMovesFunds<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.spv)]
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SpvMovesFunds<'info> {
    fn vault_to_spv(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.vault.to_account_info(),
                to: self.spv_asset.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct SpvPaysIn<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.spv)]
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SpvPaysIn<'info> {
    fn spv_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.spv_asset.to_account_info(),
                to: self.vault.to_account_info(),
                authority: self.spv.to_account_info(),
            },
        )
    }
}

#[derive(Accounts)]
pub struct ClaimCoupon<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [seeds::POSITION, pool.key().as_ref(), holder.key().as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,

    /// Read, not trusted. The scaffold took the holder's balance as an argument, which
    /// would have let anyone claim against a balance they did not hold.
    #[account(token::mint = pool.share_mint, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,

    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> ClaimCoupon<'info> {
    fn vault_to_holder(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.vault.to_account_info(),
                to: self.holder_asset.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}

/// Boxed accounts: a Solana stack frame is capped at 4KB and every `Account` is
/// deserialised into it. Eight of them overran by 136 bytes, so the larger ones live on
/// the heap and the frame holds pointers. Nothing about the semantics changes.
#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [seeds::POSITION, pool.key().as_ref(), holder.key().as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = pool.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,

    #[account(mut, token::mint = pool.share_mint, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,

    #[account(mut, token::mint = pool.asset_mint, token::authority = pool.treasury)]
    pub treasury_asset: Box<Account<'info, TokenAccount>>,

    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Withdraw<'info> {
    fn burn_shares(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Burn {
                mint: self.share_mint.to_account_info(),
                from: self.holder_shares.to_account_info(),
                authority: self.holder.to_account_info(),
            },
        )
    }

    fn vault_to_holder(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.vault.to_account_info(),
                to: self.holder_asset.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }

    fn vault_to_treasury(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(
            self.token_program.key(),
            Transfer {
                from: self.vault.to_account_info(),
                to: self.treasury_asset.to_account_info(),
                authority: self.pool.to_account_info(),
            },
        )
    }
}
