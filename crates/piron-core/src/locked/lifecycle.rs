//! Position state transitions for Locked pools.
//!
//! Ported from the redemption, early-exit and rollover paths of
//! `src/LockedPoolManager.sol` and `src/libraries/LockedPoolManagerLib.sol`.
//!
//! Each function takes a position and returns what to write plus what to pay. Burning
//! shares, moving tokens and drawing on the yield reserve belong to the binding.

use crate::error::Error;
use crate::locked::types::{InterestPayment, LockTier, Position, PositionStatus};
use crate::locked::{calc, validate};
use crate::math::{self, Amount};

/// Redeeming a matured position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Redemption {
    /// Total owed to the holder.
    pub payout: Amount,
    /// Interest included in that payout. Zero when it was taken upfront.
    pub interest_component: Amount,
    pub shares_burned: Amount,
    pub new_status: PositionStatus,
}

/// Computes a redemption at or after maturity.
pub fn redeem(position: &Position, now: u64) -> Result<Redemption, Error> {
    validate::validate_redemption(position, now)?;

    let interest_component = match position.payment_choice {
        // Already handed over at deposit; redemption returns principal only.
        InterestPayment::Upfront => 0,
        InterestPayment::AtMaturity => position.full_interest_amount,
    };

    Ok(Redemption {
        payout: position.expected_maturity_payout,
        interest_component,
        shares_burned: calc::shares_for(position.principal_deposited),
        new_status: PositionStatus::Redeemed,
    })
}

/// Leaving before maturity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlyExit {
    pub payout: Amount,
    pub penalty: Amount,
    pub interest_earned: Amount,
    pub shares_burned: Amount,
    pub new_status: PositionStatus,
}

/// Computes an early exit, penalty included.
pub fn exit_early(position: &Position, tier: &LockTier, now: u64) -> Result<EarlyExit, Error> {
    validate::validate_early_exit(position, now)?;
    let result = calc::early_exit(position, tier, now)?;

    Ok(EarlyExit {
        payout: result.payout,
        penalty: result.penalty,
        interest_earned: result.interest_earned,
        shares_burned: calc::shares_for(position.principal_deposited),
        new_status: PositionStatus::EarlyExit,
    })
}

/// Re-locking a matured position into a new term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rollover {
    /// The position that closes.
    pub closed_status: PositionStatus,
    /// The position that opens, at whatever rate the tier offers today.
    pub new_position: Position,
    /// Capital carried across: the matured payout, not the original principal.
    pub principal_rolled: Amount,
}

/// Rolls a matured position into `tier` at today's rate.
///
/// The whole matured payout is re-locked, so an at-maturity position compounds while an
/// upfront one does not — the upfront holder already took their interest out.
pub fn roll_over(
    position: &Position,
    tier: &LockTier,
    tier_index: u32,
    payment: InterestPayment,
    now: u64,
) -> Result<Rollover, Error> {
    validate::validate_rollover(position, tier, now)?;

    let principal_rolled = position.expected_maturity_payout;
    if principal_rolled < tier.min_deposit {
        return Err(Error::BelowMinInvestment);
    }

    let interest = calc::interest_for_term(principal_rolled, tier.apy_bps, tier.duration_days)?;
    if payment == InterestPayment::Upfront {
        validate::validate_upfront_capacity(principal_rolled, interest)?;
    }

    let mut new_position = calc::build_position(principal_rolled, tier, tier_index, payment, now)?;
    new_position.auto_rollover = position.auto_rollover;

    Ok(Rollover {
        closed_status: PositionStatus::RolledOver,
        new_position,
        principal_rolled,
    })
}

/// Pool-wide effect of opening a position, for updating `PoolMetrics`.
pub fn metrics_delta_on_open(position: &Position) -> Result<(Amount, Amount, Amount), Error> {
    let upfront = match position.payment_choice {
        InterestPayment::Upfront => position.full_interest_amount,
        InterestPayment::AtMaturity => 0,
    };
    let pending = math::checked_sub(position.full_interest_amount, upfront)?;
    Ok((position.principal_deposited, upfront, pending))
}
