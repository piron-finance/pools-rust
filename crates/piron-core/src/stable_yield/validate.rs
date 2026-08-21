//! Preconditions and reserve rules for Stable Yield pools.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::stable_yield::types::ReserveConfig;
use crate::BASIS_POINTS;

/// Cash the pool must keep on hand: the greater of the absolute floor and the ratio.
pub fn required_reserve(total_nav: Amount, config: &ReserveConfig) -> Result<Amount, Error> {
    if config.reserve_ratio_bps > BASIS_POINTS {
        return Err(Error::InvalidBps);
    }
    let ratio_floor = math::bps_of(total_nav, config.reserve_ratio_bps)?;
    Ok(if config.min_absolute_reserve > ratio_floor {
        config.min_absolute_reserve
    } else {
        ratio_floor
    })
}

/// Cash free to be deployed to an SPV without breaching the reserve.
///
/// Returns zero rather than a negative when the pool is already below its floor.
pub fn deployable_amount(
    pool_reserves: Amount,
    total_nav: Amount,
    config: &ReserveConfig,
) -> Result<Amount, Error> {
    let required = required_reserve(total_nav, config)?;
    if pool_reserves <= required {
        return Ok(0);
    }
    math::checked_sub(pool_reserves, required)
}

/// Checks a deployment leaves the reserve intact.
pub fn validate_deployment(
    amount: Amount,
    pool_reserves: Amount,
    total_nav: Amount,
    config: &ReserveConfig,
) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if amount > deployable_amount(pool_reserves, total_nav, config)? {
        return Err(Error::ReserveBreached);
    }
    Ok(())
}

/// Checks a deposit against the pool's terms.
pub fn validate_deposit(
    assets: Amount,
    min_investment: Amount,
    is_active: bool,
) -> Result<(), Error> {
    if !is_active {
        return Err(Error::PoolInactive);
    }
    if assets <= 0 {
        return Err(Error::InvalidAmount);
    }
    if assets < min_investment {
        return Err(Error::BelowMinInvestment);
    }
    Ok(())
}

/// Checks a holder has been in long enough to leave.
///
/// The holding period is what stops a depositor arriving the day before a coupon lands
/// and leaving the day after with a slice of it.
pub fn validate_holding_period(
    deposit_time: u64,
    holding_period_secs: u64,
    now: u64,
) -> Result<(), Error> {
    if now < deposit_time.saturating_add(holding_period_secs) {
        return Err(Error::HoldingPeriodNotMet);
    }
    Ok(())
}

/// Checks a withdrawal request is well formed.
pub fn validate_withdrawal_request(shares: Amount, user_shares: Amount) -> Result<(), Error> {
    if shares <= 0 {
        return Err(Error::InvalidAmount);
    }
    if shares > user_shares {
        return Err(Error::InsufficientShares);
    }
    Ok(())
}
