//! Account layouts for locked pools.
//!
//! The shape that differs from single-asset: there is no share token. A locked payout is
//! fixed per position at deposit, so a position is its own account rather than a balance,
//! and the tier table is a fixed array because an account is sized once at creation.

use anchor_lang::prelude::*;

use piron_core::locked::types as lt;
use piron_core::Amount;

/// How many tiers a pool can offer. Fixed because the table lives inside the pool account.
pub const MAX_TIERS: usize = 8;

#[account]
#[derive(InitSpace)]
pub struct LockedPool {
    pub pool_id: u64,
    pub authority: Pubkey,
    pub spv: Pubkey,
    pub treasury: Pubkey,
    pub asset_mint: Pubkey,
    pub vault: Pubkey,
    /// Yield reserve backing early exits. Default when the pool has none, in which case an
    /// exit the vault cannot cover fails rather than silently paying short.
    pub reserve: Pubkey,

    pub min_investment: u64,
    pub tier_count: u8,
    pub tiers: [StoredTier; MAX_TIERS],

    /// Monotonic. Position addresses derive from it, so it never rewinds.
    pub next_position_id: u64,

    // --- metrics ---
    pub total_principal_locked: u64,
    pub total_interest_committed: u64,
    pub total_interest_paid_upfront: u64,
    pub total_interest_pending_maturity: u64,
    pub total_invested_amount: u64,
    pub total_expected_maturity_payout: u64,
    pub active_positions: u32,
    pub total_positions: u32,

    /// Owed to the yield reserve for early exits the vault could not cover.
    pub reserve_debt: u64,

    pub bump: u8,
}

impl LockedPool {
    pub const LEN: usize = 8 + Self::INIT_SPACE;

    pub fn tier(&self, index: u32) -> core::result::Result<lt::LockTier, piron_core::Error> {
        let t = self
            .tiers
            .get(index as usize)
            .filter(|_| (index as usize) < self.tier_count as usize)
            .ok_or(piron_core::Error::InvalidTier)?;
        Ok(t.to_core())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct StoredTier {
    pub duration_days: u32,
    pub apy_bps: u32,
    pub early_exit_penalty_bps: u32,
    pub min_deposit: u64,
    pub is_active: bool,
}

impl StoredTier {
    pub fn to_core(&self) -> lt::LockTier {
        lt::LockTier {
            duration_days: self.duration_days,
            apy_bps: self.apy_bps,
            early_exit_penalty_bps: self.early_exit_penalty_bps,
            min_deposit: self.min_deposit as Amount,
            is_active: self.is_active,
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct LockedPosition {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub position_id: u64,

    pub principal_deposited: u64,
    pub full_interest_amount: u64,
    pub apy_bps_at_deposit: u32,
    /// 0 = upfront, 1 = at maturity.
    pub payment_choice: u8,
    pub interest_paid: bool,
    pub invested_amount: u64,
    pub expected_maturity_payout: u64,
    pub lock_start: i64,
    pub lock_end: i64,
    pub tier_index: u32,
    /// Mirrors `PositionStatus`: 0 active, 1 matured, 2 redeemed, 3 early exit, 4 rolled.
    pub status: u8,
    pub actual_payout: u64,
    pub penalty_paid: u64,
    pub interest_earned: u64,
    pub auto_rollover: bool,

    pub bump: u8,
}

impl LockedPosition {
    pub const LEN: usize = 8 + Self::INIT_SPACE;

    pub fn to_core(&self) -> lt::Position {
        lt::Position {
            principal_deposited: self.principal_deposited as Amount,
            full_interest_amount: self.full_interest_amount as Amount,
            apy_bps_at_deposit: self.apy_bps_at_deposit,
            payment_choice: if self.payment_choice == 0 {
                lt::InterestPayment::Upfront
            } else {
                lt::InterestPayment::AtMaturity
            },
            interest_paid: self.interest_paid,
            invested_amount: self.invested_amount as Amount,
            expected_maturity_payout: self.expected_maturity_payout as Amount,
            lock_start: self.lock_start.max(0) as u64,
            lock_end: self.lock_end.max(0) as u64,
            tier_index: self.tier_index,
            status: status_from_u8(self.status),
            actual_payout: self.actual_payout as Amount,
            penalty_paid: self.penalty_paid as Amount,
            interest_earned: self.interest_earned as Amount,
            auto_rollover: self.auto_rollover,
        }
    }

    pub fn apply(&mut self, p: &lt::Position) -> core::result::Result<(), piron_core::Error> {
        self.principal_deposited = to_u64(p.principal_deposited)?;
        self.full_interest_amount = to_u64(p.full_interest_amount)?;
        self.apy_bps_at_deposit = p.apy_bps_at_deposit;
        self.payment_choice = match p.payment_choice {
            lt::InterestPayment::Upfront => 0,
            lt::InterestPayment::AtMaturity => 1,
        };
        self.interest_paid = p.interest_paid;
        self.invested_amount = to_u64(p.invested_amount)?;
        self.expected_maturity_payout = to_u64(p.expected_maturity_payout)?;
        self.lock_start = p.lock_start as i64;
        self.lock_end = p.lock_end as i64;
        self.tier_index = p.tier_index;
        self.status = status_to_u8(p.status);
        self.actual_payout = to_u64(p.actual_payout)?;
        self.penalty_paid = to_u64(p.penalty_paid)?;
        self.interest_earned = to_u64(p.interest_earned)?;
        self.auto_rollover = p.auto_rollover;
        Ok(())
    }
}

pub fn status_from_u8(v: u8) -> lt::PositionStatus {
    match v {
        0 => lt::PositionStatus::Active,
        1 => lt::PositionStatus::Matured,
        2 => lt::PositionStatus::Redeemed,
        3 => lt::PositionStatus::EarlyExit,
        _ => lt::PositionStatus::RolledOver,
    }
}

pub fn status_to_u8(s: lt::PositionStatus) -> u8 {
    match s {
        lt::PositionStatus::Active => 0,
        lt::PositionStatus::Matured => 1,
        lt::PositionStatus::Redeemed => 2,
        lt::PositionStatus::EarlyExit => 3,
        lt::PositionStatus::RolledOver => 4,
    }
}

/// Narrows a core amount back into the `u64` a token and an account field hold.
///
/// Same reasoning as the single-asset binding: core works in `i128` so intermediate
/// products cannot wrap, and `as u64` would throw that away by truncating.
pub fn to_u64(value: Amount) -> core::result::Result<u64, piron_core::Error> {
    if value < 0 {
        return Err(piron_core::Error::NegativeAmount);
    }
    u64::try_from(value).map_err(|_| piron_core::Error::Overflow)
}
