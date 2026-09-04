//! Locked pool — Solana binding layer.
//!
//! The same entry points as the Soroban binding. Every protocol decision comes from
//! `piron-core`; nothing here decides anything.
//!
//! What differs from the single-asset binding on this chain:
//!
//! - **No share token.** A locked payout is fixed per position at deposit, so a position
//!   is its own account rather than a balance. Nothing is minted and nothing is fungible.
//! - **Tiers live in the pool account**, as a fixed array, because an account is sized
//!   once at creation.
//! - **Early exit can outrun the vault.** The payout carries interest accrued to today and
//!   the vault holds only principal until the SPV returns yield, so the exit is normally
//!   short by exactly the interest earned. The yield reserve lends the difference.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

pub mod seeds;
pub mod state;

use piron_core::locked::{calc, lifecycle, types as lt, validate};
use state::{to_u64, LockedPool, LockedPosition, StoredTier, MAX_TIERS};

declare_id!("PironLock1111111111111111111111111111111111");

fn map_err(err: piron_core::Error) -> Error {
    Error::from(ProgramError::Custom(err.code()))
}

fn clock_now() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| map_err(piron_core::Error::Overflow))
}

#[program]
pub mod piron_locked {
    use super::*;

    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        pool_id: u64,
        spv: Pubkey,
        treasury: Pubkey,
        reserve: Pubkey,
        min_investment: u64,
    ) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.pool_id = pool_id;
        pool.authority = ctx.accounts.authority.key();
        pool.spv = spv;
        pool.treasury = treasury;
        pool.reserve = reserve;
        pool.asset_mint = ctx.accounts.asset_mint.key();
        pool.vault = ctx.accounts.vault.key();
        pool.min_investment = min_investment;
        pool.next_position_id = 1;
        pool.bump = ctx.bumps.pool;
        emit!(PoolInitialized { pool: pool.key(), pool_id });
        Ok(())
    }

    /// Adds a lock tier. Terms are fixed per position at deposit, so changing a tier
    /// later never reprices a position that already exists.
    pub fn add_tier(ctx: Context<Authority>, tier: StoredTier) -> Result<u32> {
        validate::validate_tier(&tier.to_core()).map_err(map_err)?;
        let pool = &mut ctx.accounts.pool;
        let index = pool.tier_count as usize;
        if index >= MAX_TIERS {
            return Err(map_err(piron_core::Error::InvalidTier));
        }
        pool.tiers[index] = tier;
        pool.tier_count += 1;
        emit!(TierAdded { pool: pool.key(), index: index as u32 });
        Ok(index as u32)
    }

    /// Locks capital into a tier. `payment_choice`: 0 upfront, 1 at maturity.
    pub fn deposit(
        ctx: Context<Deposit>,
        amount: u64,
        tier_index: u32,
        payment_choice: u8,
    ) -> Result<u64> {
        let now = clock_now()?;
        let pool = &ctx.accounts.pool;
        let tier = pool.tier(tier_index).map_err(map_err)?;
        validate::validate_deposit(amount as i128, tier_index, &tier, pool.tier_count as u32)
            .map_err(map_err)?;
        if (amount as i128) < pool.min_investment as i128 {
            return Err(map_err(piron_core::Error::BelowMinInvestment));
        }

        let payment = if payment_choice == 0 {
            lt::InterestPayment::Upfront
        } else {
            lt::InterestPayment::AtMaturity
        };
        let built = calc::build_position(amount as i128, &tier, tier_index, payment, now)
            .map_err(map_err)?;

        token::transfer(ctx.accounts.holder_to_vault(), amount)?;

        let position_id = ctx.accounts.pool.next_position_id;
        let position = &mut ctx.accounts.position;
        position.pool = ctx.accounts.pool.key();
        position.owner = ctx.accounts.holder.key();
        position.position_id = position_id;
        position.bump = ctx.bumps.position;
        position.apply(&built).map_err(map_err)?;

        let (principal, committed, invested) =
            lifecycle::metrics_delta_on_open(&built).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.next_position_id = add(position_id, 1)?;
        pool.total_principal_locked = add(pool.total_principal_locked, to_u64(principal).map_err(map_err)?)?;
        pool.total_interest_committed = add(pool.total_interest_committed, to_u64(committed).map_err(map_err)?)?;
        pool.total_invested_amount = add(pool.total_invested_amount, to_u64(invested).map_err(map_err)?)?;
        pool.total_expected_maturity_payout =
            add(pool.total_expected_maturity_payout, to_u64(built.expected_maturity_payout).map_err(map_err)?)?;
        if matches!(built.payment_choice, lt::InterestPayment::AtMaturity) {
            pool.total_interest_pending_maturity =
                add(pool.total_interest_pending_maturity, to_u64(built.full_interest_amount).map_err(map_err)?)?;
        }
        pool.active_positions += 1;
        pool.total_positions += 1;

        emit!(PositionOpened {
            pool: pool.key(),
            owner: ctx.accounts.holder.key(),
            position_id,
            principal: amount,
            tier_index,
        });
        Ok(position_id)
    }

    /// Redeems a matured position at its fixed payout.
    pub fn redeem(ctx: Context<Settle>) -> Result<u64> {
        let now = clock_now()?;
        let core_position = ctx.accounts.position.to_core();
        let outcome = lifecycle::redeem(&core_position, now).map_err(map_err)?;
        let payout = to_u64(outcome.payout).map_err(map_err)?;

        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_holder().with_signer(signer), payout)?;

        let position = &mut ctx.accounts.position;
        position.status = state::status_to_u8(outcome.new_status);
        position.actual_payout = payout;

        let pool = &mut ctx.accounts.pool;
        pool.active_positions = pool.active_positions.saturating_sub(1);
        pool.total_principal_locked = pool
            .total_principal_locked
            .saturating_sub(ctx.accounts.position.principal_deposited);

        emit!(PositionRedeemed {
            pool: pool.key(),
            owner: ctx.accounts.holder.key(),
            position_id: ctx.accounts.position.position_id,
            payout,
        });
        Ok(payout)
    }

    /// Exits before maturity, paying accrued interest less the tier's penalty.
    ///
    /// The vault holds only principal until the SPV returns yield, so the exit is normally
    /// short by exactly the interest earned — the ordinary case, not an edge one. The
    /// reserve lends the difference and the pool owes it back.
    pub fn exit_early(ctx: Context<ExitEarly>) -> Result<u64> {
        let now = clock_now()?;
        let core_position = ctx.accounts.position.to_core();
        let tier = ctx.accounts.pool.tier(ctx.accounts.position.tier_index).map_err(map_err)?;
        let exit = lifecycle::exit_early(&core_position, &tier, now).map_err(map_err)?;

        let payout = to_u64(exit.payout).map_err(map_err)?;
        let penalty = to_u64(exit.penalty).map_err(map_err)?;
        let needed = add(payout, penalty)?;
        let held = ctx.accounts.vault.amount;

        if needed > held {
            let short = needed - held;
            if ctx.accounts.pool.reserve == Pubkey::default() {
                return Err(map_err(piron_core::Error::InsufficientReserve));
            }
            if ctx.accounts.reserve.key() != ctx.accounts.pool.reserve {
                return Err(Error::from(ProgramError::InvalidAccountData));
            }
            token::transfer(ctx.accounts.reserve_to_vault(), short)?;
            let pool = &mut ctx.accounts.pool;
            pool.reserve_debt = add(pool.reserve_debt, short)?;
            emit!(ReserveBorrowed { pool: pool.key(), amount: short, outstanding: pool.reserve_debt });
        }

        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_holder().with_signer(signer), payout)?;
        if penalty > 0 {
            // The penalty leaves the pool rather than staying in it, which is what makes
            // an early exit fund the protocol instead of subsidising the holders who stayed.
            token::transfer(ctx.accounts.vault_to_treasury().with_signer(signer), penalty)?;
        }

        let principal = ctx.accounts.position.principal_deposited;
        let position = &mut ctx.accounts.position;
        position.status = state::status_to_u8(exit.new_status);
        position.actual_payout = payout;
        position.penalty_paid = penalty;
        position.interest_earned = to_u64(exit.interest_earned).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.active_positions = pool.active_positions.saturating_sub(1);
        pool.total_principal_locked = pool.total_principal_locked.saturating_sub(principal);

        emit!(PositionExitedEarly {
            pool: pool.key(),
            owner: ctx.accounts.holder.key(),
            position_id: ctx.accounts.position.position_id,
            payout,
            penalty,
        });
        Ok(payout)
    }

    /// Repays the reserve out of the SPV's own funds.
    ///
    /// The SPV holds the returned capital, so it is the party that can settle. Leaving the
    /// debt outstanding would have the reserve subsidising this pool's holders at every
    /// other pool's expense.
    pub fn settle_debt(ctx: Context<SettleDebt>, amount: u64) -> Result<()> {
        if amount == 0 || amount > ctx.accounts.pool.reserve_debt {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        token::transfer(ctx.accounts.spv_to_reserve(), amount)?;
        let pool = &mut ctx.accounts.pool;
        pool.reserve_debt -= amount;
        emit!(ReserveRepaid { pool: pool.key(), amount, outstanding: pool.reserve_debt });
        Ok(())
    }

    /// Sends capital to the SPV to invest.
    ///
    /// The whole vault is drawable. Holding back principal plus the interest owed would
    /// leave the pool nothing to invest, and the yield paying the promised APY has to come
    /// from somewhere; the obligation is met at maturity out of what the SPV returns.
    pub fn deploy_to_spv(ctx: Context<SpvMovesFunds>, amount: u64) -> Result<()> {
        if amount == 0 || amount > ctx.accounts.vault.amount {
            return Err(map_err(piron_core::Error::InsufficientLiquidity));
        }
        let (pool_id, bump) = (ctx.accounts.pool.pool_id, ctx.accounts.pool.bump);
        let id_bytes = pool_id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_spv().with_signer(signer), amount)?;
        emit!(CapitalDeployed { pool: ctx.accounts.pool.key(), amount });
        Ok(())
    }

    /// Returns capital or yield from the SPV.
    pub fn return_from_spv(ctx: Context<SpvMovesFunds>, amount: u64) -> Result<()> {
        if amount == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        token::transfer(ctx.accounts.spv_to_vault(), amount)?;
        emit!(CapitalReturned { pool: ctx.accounts.pool.key(), amount });
        Ok(())
    }
}

// ---------------------------------------------------------------- contexts

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct InitializePool<'info> {
    #[account(init, payer = authority, space = LockedPool::LEN,
              seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    pub asset_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = authority, seeds = [seeds::VAULT, pool.key().as_ref()], bump,
              token::mint = asset_mint, token::authority = pool)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Authority<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    #[account(address = pool.authority)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    #[account(init, payer = holder, space = LockedPosition::LEN,
              seeds = [seeds::POSITION, pool.key().as_ref(), &pool.next_position_id.to_le_bytes()],
              bump)]
    pub position: Box<Account<'info, LockedPosition>>,
    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
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
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    #[account(mut, seeds = [seeds::POSITION, pool.key().as_ref(), &position.position_id.to_le_bytes()],
              bump = position.bump, has_one = owner)]
    pub position: Box<Account<'info, LockedPosition>>,
    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
    #[account(address = position.owner)]
    pub holder: Signer<'info>,
    /// CHECK: matched against `position.owner` by `has_one`.
    pub owner: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Settle<'info> {
    fn vault_to_holder(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.holder_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
}

#[derive(Accounts)]
pub struct ExitEarly<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    #[account(mut, seeds = [seeds::POSITION, pool.key().as_ref(), &position.position_id.to_le_bytes()],
              bump = position.bump, has_one = owner)]
    pub position: Box<Account<'info, LockedPosition>>,
    #[account(mut, address = pool.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint)]
    pub treasury_asset: Box<Account<'info, TokenAccount>>,
    /// The reserve's token account, and whoever may sign for it. Only touched when the
    /// vault cannot cover the exit.
    #[account(mut)]
    pub reserve_asset: Box<Account<'info, TokenAccount>>,
    /// CHECK: compared against `pool.reserve` before any transfer.
    pub reserve: UncheckedAccount<'info>,
    pub reserve_authority: Signer<'info>,
    #[account(address = position.owner)]
    pub holder: Signer<'info>,
    /// CHECK: matched against `position.owner` by `has_one`.
    pub owner: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> ExitEarly<'info> {
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
    fn reserve_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.reserve_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.reserve_authority.to_account_info(),
        })
    }
}

#[derive(Accounts)]
pub struct SettleDebt<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
    #[account(mut, token::mint = pool.asset_mint, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.asset_mint)]
    pub reserve_asset: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.spv)]
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SettleDebt<'info> {
    fn spv_to_reserve(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.spv_asset.to_account_info(),
            to: self.reserve_asset.to_account_info(),
            authority: self.spv.to_account_info(),
        })
    }
}

#[derive(Accounts)]
pub struct SpvMovesFunds<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Box<Account<'info, LockedPool>>,
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

// ---------------------------------------------------------------- events

#[event] pub struct PoolInitialized { pub pool: Pubkey, pub pool_id: u64 }
#[event] pub struct TierAdded { pub pool: Pubkey, pub index: u32 }
#[event] pub struct PositionOpened { pub pool: Pubkey, pub owner: Pubkey, pub position_id: u64, pub principal: u64, pub tier_index: u32 }
#[event] pub struct PositionRedeemed { pub pool: Pubkey, pub owner: Pubkey, pub position_id: u64, pub payout: u64 }
#[event] pub struct PositionExitedEarly { pub pool: Pubkey, pub owner: Pubkey, pub position_id: u64, pub payout: u64, pub penalty: u64 }
#[event] pub struct ReserveBorrowed { pub pool: Pubkey, pub amount: u64, pub outstanding: u64 }
#[event] pub struct ReserveRepaid { pub pool: Pubkey, pub amount: u64, pub outstanding: u64 }
#[event] pub struct CapitalDeployed { pub pool: Pubkey, pub amount: u64 }
#[event] pub struct CapitalReturned { pub pool: Pubkey, pub amount: u64 }
