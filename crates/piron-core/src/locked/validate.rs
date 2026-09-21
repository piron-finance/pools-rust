//! Preconditions for Locked pool operations.
//!
//! Ported from the validation half of `src/libraries/LockedPoolLibrary.sol`, which returns
//! `(bool, string)` pairs. Typed errors here — same conditions, same order.

use crate::error::Error;
use crate::locked::types::{LockTier, Position, PositionStatus};
use crate::math::Amount;
use crate::BASIS_POINTS;

/// Ceiling on both APY and early-exit penalty: 50%.
///
/// A guard against fat-fingered tier configuration, not an economic view.
pub const MAX_TIER_BPS: u32 = 5_000;

/// Longest term a tier may offer: ten years.
///
/// Same purpose as the bps ceilings — a guard against a mistyped tier locking capital
/// past any horizon the protocol plans for, not a view on what terms are sensible.
pub const MAX_TIER_DURATION_DAYS: u32 = 3_650;

/// Checks a tier is offerable.
pub fn validate_tier(tier: &LockTier) -> Result<(), Error> {
    if tier.duration_days == 0 || tier.duration_days > MAX_TIER_DURATION_DAYS {
        return Err(Error::InvalidTierDuration);
    }
    if tier.apy_bps > MAX_TIER_BPS {
        return Err(Error::TierApyTooHigh);
    }
    if tier.early_exit_penalty_bps > MAX_TIER_BPS {
        return Err(Error::TierPenaltyTooHigh);
    }
    if tier.min_deposit < 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

/// Checks a deposit against the tier it is going into.
pub fn validate_deposit(
    amount: Amount,
    tier_index: u32,
    tier: &LockTier,
    tier_count: u32,
) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if tier_index >= tier_count {
        return Err(Error::InvalidTier);
    }
    if !tier.is_active {
        return Err(Error::TierInactive);
    }
    if amount < tier.min_deposit {
        return Err(Error::BelowMinInvestment);
    }
    Ok(())
}

/// An upfront-interest deposit must cover its own interest.
///
/// PARITY: the Solidity computes `principal - interest` in `calculateInvestedAmount` and
/// reverts on 0.8 underflow if interest exceeds principal. That cannot happen with the
/// 50% APY ceiling below roughly a two-year tenor, but the check is explicit here rather
/// than implied by a subtraction.
pub fn validate_upfront_capacity(principal: Amount, interest: Amount) -> Result<(), Error> {
    if interest > principal {
        return Err(Error::InterestExceedsPrincipal);
    }
    Ok(())
}

/// Checks a position can be redeemed at maturity.
pub fn validate_redemption(position: &Position, now: u64) -> Result<(), Error> {
    match position.status {
        PositionStatus::Redeemed => return Err(Error::PositionAlreadyRedeemed),
        PositionStatus::EarlyExit => return Err(Error::PositionAlreadyExited),
        _ => {}
    }
    if now < position.lock_end {
        return Err(Error::PositionNotMatured);
    }
    if !matches!(
        position.status,
        PositionStatus::Active | PositionStatus::Matured
    ) {
        return Err(Error::PositionNotRedeemable);
    }
    Ok(())
}

/// Checks a position can be exited before maturity.
pub fn validate_early_exit(position: &Position, now: u64) -> Result<(), Error> {
    match position.status {
        PositionStatus::Redeemed => return Err(Error::PositionAlreadyRedeemed),
        PositionStatus::EarlyExit => return Err(Error::PositionAlreadyExited),
        _ => {}
    }
    if now >= position.lock_end {
        return Err(Error::PositionAlreadyMatured);
    }
    if position.status != PositionStatus::Active {
        return Err(Error::PositionNotActive);
    }
    Ok(())
}

/// A rollover re-locks a matured position into a tier that is still on offer.
pub fn validate_rollover(position: &Position, tier: &LockTier, now: u64) -> Result<(), Error> {
    validate_redemption(position, now)?;
    if !tier.is_active {
        return Err(Error::TierInactive);
    }
    validate_tier(tier)
}

const _: () = assert!(MAX_TIER_BPS <= BASIS_POINTS);
