//! Single-Asset pool — Solana binding layer.
//!
//! Scaffold. The same three entry points as the Soroban binding, so the two can be read
//! side by side: `record_deposit`, `record_coupon`, `claim_coupon`. Every protocol
//! decision is delegated to `piron-core`; nothing here decides anything.
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
//! - **Money is `u64`.** Widening into core is free; narrowing on the way out is checked.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

extern crate alloc;

pub mod state;

use piron_core::single_asset::{calc, validate};
use state::{Pool, Position, POOL_SEED, POSITION_SEED};

declare_id!("Piron11111111111111111111111111111111111111");

/// Maps a core error onto a raw custom program error.
///
/// Deliberately **not** an Anchor `#[error_code]` enum: Anchor offsets custom errors by
/// 6000, which would make the same protocol condition report a different number on Solana
/// than on Stellar. Returning `ProgramError::Custom` with the core code verbatim keeps one
/// taxonomy across all three runtimes, which is what the backend maps on.
fn map_err(err: piron_core::Error) -> Error {
    Error::from(ProgramError::Custom(err.code()))
}

#[program]
pub mod piron_single_asset {
    use super::*;

    /// Records a deposit made during the funding window.
    pub fn record_deposit(ctx: Context<RecordDeposit>, assets: u64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp.max(0) as u64;
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;

        validate::validate_deposit(&data, assets as i128, now).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.total_raised = pool
            .total_raised
            .checked_add(assets)
            .ok_or_else(|| map_err(piron_core::Error::Overflow))?;

        let position = &mut ctx.accounts.position;
        if position.deposit_time == 0 {
            position.pool = pool.key();
            position.owner = ctx.accounts.investor.key();
            position.deposit_time = now as i64;
            position.bump = ctx.bumps.position;
        }

        emit!(DepositRecorded {
            pool: pool.key(),
            investor: ctx.accounts.investor.key(),
            assets,
            total_raised: pool.total_raised,
        });
        Ok(())
    }

    /// Records a coupon received from the SPV.
    pub fn record_coupon(ctx: Context<RecordCoupon>, amount: u64) -> Result<()> {
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        validate::validate_coupon_payment(&data, amount as i128).map_err(map_err)?;

        let pool = &mut ctx.accounts.pool;
        pool.total_coupons_received = pool
            .total_coupons_received
            .checked_add(amount)
            .ok_or_else(|| map_err(piron_core::Error::Overflow))?;

        emit!(CouponReceived {
            pool: pool.key(),
            amount,
            total_received: pool.total_coupons_received,
        });
        Ok(())
    }

    /// Claims a holder's share of distributed coupons.
    pub fn claim_coupon(ctx: Context<ClaimCoupon>, user_shares: u64, total_shares: u64) -> Result<u64> {
        // TODO(vault): read shares from the vault token account rather than trusting
        // arguments. Same shortcut, same TODO, as the Soroban scaffold.
        let data = ctx.accounts.pool.to_core().map_err(map_err)?;
        let position = &ctx.accounts.position;

        let claim = calc::claim_coupon(
            &data,
            user_shares as i128,
            total_shares as i128,
            position.coupons_claimed as i128,
        )
        .map_err(map_err)?;

        // Narrowing back to u64 is the one direction that can fail. Core works in i128;
        // a value that does not fit a token amount is a bug, not a rounding artifact.
        let claimable = u64::try_from(claim.claimable)
            .map_err(|_| map_err(piron_core::Error::Overflow))?;
        let new_total = u64::try_from(claim.new_claimed_total)
            .map_err(|_| map_err(piron_core::Error::Overflow))?;

        ctx.accounts.position.coupons_claimed = new_total;

        let pool = &mut ctx.accounts.pool;
        pool.total_coupons_claimed = pool
            .total_coupons_claimed
            .checked_add(claimable)
            .ok_or_else(|| map_err(piron_core::Error::Overflow))?;

        emit!(CouponClaimed {
            pool: pool.key(),
            holder: ctx.accounts.holder.key(),
            claimable,
        });
        Ok(claimable)
    }
}

// ---------------------------------------------------------------- contexts

#[derive(Accounts)]
pub struct RecordDeposit<'info> {
    #[account(mut, seeds = [POOL_SEED, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    #[account(
        init_if_needed,
        payer = investor,
        space = Position::LEN,
        seeds = [POSITION_SEED, pool.key().as_ref(), investor.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,

    /// The analogue of `investor.require_auth()` on Soroban.
    #[account(mut)]
    pub investor: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RecordCoupon<'info> {
    #[account(mut, seeds = [POOL_SEED, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    // TODO(access): constrain to the operator role rather than the pool authority once
    // the access-control program exists.
    #[account(address = pool.authority)]
    pub operator: Signer<'info>,
}

#[derive(Accounts)]
pub struct ClaimCoupon<'info> {
    #[account(mut, seeds = [POOL_SEED, &pool.pool_id.to_le_bytes()], bump = pool.bump)]
    pub pool: Account<'info, Pool>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), holder.key().as_ref()],
        bump = position.bump,
        has_one = owner @ ProgramError::InvalidAccountData
    )]
    pub position: Account<'info, Position>,

    pub holder: Signer<'info>,

    /// CHECK: matched against `position.owner` by the `has_one` constraint above.
    pub owner: AccountInfo<'info>,
}

// ---------------------------------------------------------------- events

#[event]
pub struct DepositRecorded {
    pub pool: Pubkey,
    pub investor: Pubkey,
    pub assets: u64,
    pub total_raised: u64,
}

#[event]
pub struct CouponReceived {
    pub pool: Pubkey,
    pub amount: u64,
    pub total_received: u64,
}

#[event]
pub struct CouponClaimed {
    pub pool: Pubkey,
    pub holder: Pubkey,
    pub claimable: u64,
}
