//! Interest, penalty and payout arithmetic for Locked pools.
//!
//! Ported from `src/libraries/LockedPoolLibrary.sol`.

use crate::error::Error;
use crate::locked::types::{
    EarlyExitCalculation, InterestPayment, LockTier, Position, PositionStatus, PositionSummary,
};
use crate::math::{self, Amount};
use crate::BASIS_POINTS;

pub const DAYS_PER_YEAR: u32 = 365;
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Simple interest for a whole term: `principal * apy * days / (10_000 * 365)`.
///
/// Simple, not compound — a 12% APY over 180 days pays 6%, not 5.83%.
pub fn interest_for_term(
    principal: Amount,
    apy_bps: u32,
    duration_days: u32,
) -> Result<Amount, Error> {
    let numerator = (apy_bps as Amount)
        .checked_mul(duration_days as Amount)
        .ok_or(Error::Overflow)?;
    let denominator = (BASIS_POINTS as Amount)
        .checked_mul(DAYS_PER_YEAR as Amount)
        .ok_or(Error::Overflow)?;
    math::mul_div(principal, numerator, denominator)
}

/// The share of `full_interest` earned by `elapsed` of `total_duration`.
pub fn pro_rata_interest(
    full_interest: Amount,
    elapsed: u64,
    total_duration: u64,
) -> Result<Amount, Error> {
    if total_duration == 0 {
        return Ok(0);
    }
    if elapsed >= total_duration {
        return Ok(full_interest);
    }
    math::mul_div(full_interest, elapsed as Amount, total_duration as Amount)
}

/// What goes to work: principal, less interest already handed over on an upfront position.
pub fn invested_amount(
    principal: Amount,
    interest: Amount,
    payment: InterestPayment,
) -> Result<Amount, Error> {
    match payment {
        InterestPayment::Upfront => math::checked_sub(principal, interest),
        InterestPayment::AtMaturity => Ok(principal),
    }
}

/// What redemption pays: principal alone if interest was taken upfront, else both.
pub fn expected_maturity_payout(
    principal: Amount,
    interest: Amount,
    payment: InterestPayment,
) -> Result<Amount, Error> {
    match payment {
        InterestPayment::Upfront => Ok(principal),
        InterestPayment::AtMaturity => math::checked_add(principal, interest),
    }
}

/// Builds a position from a deposit against a tier.
pub fn build_position(
    principal: Amount,
    tier: &LockTier,
    tier_index: u32,
    payment: InterestPayment,
    now: u64,
) -> Result<Position, Error> {
    let interest = interest_for_term(principal, tier.apy_bps, tier.duration_days)?;
    let lock_end = now
        .checked_add((tier.duration_days as u64) * SECONDS_PER_DAY)
        .ok_or(Error::Overflow)?;
    let paid_upfront = payment == InterestPayment::Upfront;

    Ok(Position {
        principal_deposited: principal,
        full_interest_amount: interest,
        apy_bps_at_deposit: tier.apy_bps,
        payment_choice: payment,
        interest_paid: paid_upfront,
        invested_amount: invested_amount(principal, interest, payment)?,
        expected_maturity_payout: expected_maturity_payout(principal, interest, payment)?,
        lock_start: now,
        lock_end,
        tier_index,
        status: PositionStatus::Active,
        actual_payout: 0,
        penalty_paid: 0,
        interest_earned: if paid_upfront { interest } else { 0 },
        auto_rollover: false,
    })
}

/// Early exit on a position whose interest was already paid.
///
/// The penalty is struck on the full value the holder received — principal plus the
/// interest they were handed at deposit — and then taken out of what is left invested. A
/// holder who exits early can therefore surrender the entire remaining balance and walk
/// away with nothing beyond the interest they already hold.
pub fn early_exit_upfront(
    position: &Position,
    tier: &LockTier,
) -> Result<EarlyExitCalculation, Error> {
    let total_value_received =
        math::checked_add(position.invested_amount, position.full_interest_amount)?;
    let mut penalty = math::bps_of(total_value_received, tier.early_exit_penalty_bps)?;

    let payout = if penalty >= position.invested_amount {
        penalty = position.invested_amount;
        0
    } else {
        math::checked_sub(position.invested_amount, penalty)?
    };

    Ok(EarlyExitCalculation {
        payout,
        penalty,
        interest_earned: position.full_interest_amount,
        value_at_exit: total_value_received,
    })
}

/// Early exit on a position whose interest is owed at maturity.
///
/// Interest accrues linearly, then the penalty is taken off principal plus what accrued.
pub fn early_exit_at_maturity(
    position: &Position,
    tier: &LockTier,
    now: u64,
) -> Result<EarlyExitCalculation, Error> {
    // PARITY: the Solidity computes `currentTime - position.lockStart` and
    // `lockEnd - lockStart` directly, reverting on 0.8 underflow if called before the
    // lock started. Saturating makes an early query read as "nothing accrued".
    let elapsed = now.saturating_sub(position.lock_start);
    let total_duration = position.lock_end.saturating_sub(position.lock_start);

    let earned = pro_rata_interest(position.full_interest_amount, elapsed, total_duration)?;
    let value_at_exit = math::checked_add(position.principal_deposited, earned)?;
    let penalty = math::bps_of(value_at_exit, tier.early_exit_penalty_bps)?;

    Ok(EarlyExitCalculation {
        payout: math::checked_sub(value_at_exit, penalty)?,
        penalty,
        interest_earned: earned,
        value_at_exit,
    })
}

/// Routes to the right early-exit rule for the position's interest mode.
pub fn early_exit(
    position: &Position,
    tier: &LockTier,
    now: u64,
) -> Result<EarlyExitCalculation, Error> {
    match position.payment_choice {
        InterestPayment::Upfront => early_exit_upfront(position, tier),
        InterestPayment::AtMaturity => early_exit_at_maturity(position, tier, now),
    }
}

/// Whole days left on the lock, floored.
pub fn days_remaining(lock_end: u64, now: u64) -> u64 {
    if now >= lock_end {
        return 0;
    }
    (lock_end - now) / SECONDS_PER_DAY
}

pub fn can_redeem(position: &Position, now: u64) -> bool {
    now >= position.lock_end && position.status == PositionStatus::Active
}

pub fn can_early_exit(position: &Position, now: u64) -> bool {
    now < position.lock_end && position.status == PositionStatus::Active
}

/// A holder's view of their position right now.
pub fn position_summary(position: &Position, now: u64) -> Result<PositionSummary, Error> {
    let interest_amount = if position.payment_choice == InterestPayment::AtMaturity
        && now < position.lock_end
    {
        pro_rata_interest(
            position.full_interest_amount,
            now.saturating_sub(position.lock_start),
            position.lock_end.saturating_sub(position.lock_start),
        )?
    } else {
        position.full_interest_amount
    };

    Ok(PositionSummary {
        principal_deposited: position.principal_deposited,
        interest_amount,
        apy_bps: position.apy_bps_at_deposit,
        interest_paid_upfront: position.interest_paid,
        expected_maturity_payout: position.expected_maturity_payout,
        days_remaining: days_remaining(position.lock_end, now),
        can_redeem: can_redeem(position, now),
        can_early_exit: can_early_exit(position, now),
    })
}

/// Shares minted for a deposit. One-to-one with principal, as on EVM.
pub fn shares_for(principal: Amount) -> Amount {
    principal
}
