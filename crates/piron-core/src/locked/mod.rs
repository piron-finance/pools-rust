//! Locked (fixed-term) pools: discrete deposits at a rate fixed on the day.
//!
//! Ported from `src/libraries/LockedPoolLibrary.sol`, `src/LockedPoolManager.sol` and
//! `src/types/ILockedPoolTypes.sol`.
//!
//! Unlike Single Asset, a locked pool is not one deal with many holders — it is many
//! independent positions sharing a pool of tiers. Each carries its own term, rate, and
//! maturity, so almost everything here operates on a single `Position` rather than on
//! pool-wide state.

pub mod calc;
pub mod lifecycle;
pub mod types;
pub mod validate;

pub use calc::{
    build_position, can_early_exit, can_redeem, days_remaining, early_exit, early_exit_at_maturity,
    early_exit_upfront, expected_maturity_payout, interest_for_term, invested_amount,
    position_summary, pro_rata_interest, shares_for, DAYS_PER_YEAR, SECONDS_PER_DAY,
};
pub use lifecycle::{redeem, roll_over, exit_early, Redemption, EarlyExit, Rollover};
pub use types::{
    AllocationStatus, DebtPosition, EarlyExitCalculation, InterestPayment, LockTier, PoolMetrics,
    Position, PositionStatus, PositionSummary, SpvAllocation,
};
pub use validate::{
    validate_deposit, validate_early_exit, validate_redemption, validate_rollover, validate_tier,
    validate_upfront_capacity, MAX_TIER_BPS,
};
