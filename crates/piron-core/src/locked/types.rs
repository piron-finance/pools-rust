//! Types for Locked (fixed-term) pools.
//!
//! Ported from `src/types/ILockedPoolTypes.sol`.

use crate::math::Amount;

/// When the depositor takes their interest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterestPayment {
    /// Paid at deposit, deducted from what goes to work.
    Upfront,
    /// Paid on redemption, on top of principal.
    AtMaturity,
}

/// Lifecycle of one deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionStatus {
    Active,
    Matured,
    Redeemed,
    EarlyExit,
    RolledOver,
}

/// Lifecycle of capital sent to an SPV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationStatus {
    Pending,
    Invested,
    Returned,
    Matured,
    Cancelled,
}

/// One offered tenor: how long, at what rate, with what exit cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockTier {
    pub duration_days: u32,
    pub apy_bps: u32,
    pub early_exit_penalty_bps: u32,
    pub min_deposit: Amount,
    pub is_active: bool,
}

/// A single deposit and everything owed against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub principal_deposited: Amount,
    /// Interest for the whole term, fixed at deposit.
    pub full_interest_amount: Amount,
    /// The rate at deposit, kept because tiers can change afterwards.
    pub apy_bps_at_deposit: u32,
    pub payment_choice: InterestPayment,
    pub interest_paid: bool,
    /// What actually went to work. Principal less interest on an upfront position.
    pub invested_amount: Amount,
    pub expected_maturity_payout: Amount,
    /// Unix seconds.
    pub lock_start: u64,
    pub lock_end: u64,
    pub tier_index: u32,
    pub status: PositionStatus,
    pub actual_payout: Amount,
    pub penalty_paid: Amount,
    pub interest_earned: Amount,
    pub auto_rollover: bool,
}

/// Pool-wide totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolMetrics {
    pub total_principal_locked: Amount,
    pub total_interest_committed: Amount,
    pub total_interest_paid_upfront: Amount,
    pub total_interest_pending_maturity: Amount,
    pub total_invested_amount: Amount,
    pub total_expected_maturity_payout: Amount,
    pub active_positions: u32,
    pub total_positions: u32,
}

/// What leaving early costs and pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlyExitCalculation {
    pub payout: Amount,
    pub penalty: Amount,
    pub interest_earned: Amount,
    /// Position value before the penalty is taken.
    pub value_at_exit: Amount,
}

/// Read-only view for a holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionSummary {
    pub principal_deposited: Amount,
    /// Full interest for an upfront position; accrued-so-far for at-maturity.
    pub interest_amount: Amount,
    pub apy_bps: u32,
    pub interest_paid_upfront: bool,
    pub expected_maturity_payout: Amount,
    pub days_remaining: u64,
    pub can_redeem: bool,
    pub can_early_exit: bool,
}

/// Capital placed with an SPV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpvAllocation {
    pub amount: Amount,
    pub returned_amount: Amount,
    pub created_at: u64,
    pub status: AllocationStatus,
}

/// What the pool owes an exiting holder when liquidity is short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebtPosition {
    pub amount_owed: Amount,
    pub reserve_loan: Amount,
    pub pending_penalty: Amount,
    pub exit_time: u64,
    pub settled: bool,
}
