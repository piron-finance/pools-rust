//! Valuation and coupon arithmetic for Single-Asset pools.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::types::{PoolConfig, PoolData, PoolStatus};
use crate::BASIS_POINTS;

/// How far either side of a scheduled coupon date a payment still counts as on-time.
pub const COUPON_DATE_TOLERANCE_SECS: u64 = 24 * 60 * 60;

/// Mark-to-market value of the pool right now.
///
/// Discounted instruments accrue their discount linearly from the close of funding to
/// maturity, then sit at face value. Interest-bearing instruments are principal plus
/// whatever coupons have actually arrived.
pub fn current_pool_value(data: &PoolData, now: u64) -> Result<Amount, Error> {
    let base = data.actual_invested;

    if !data.is_discounted() {
        return math::checked_add(base, data.total_coupons_received);
    }

    if now >= data.config.maturity_date {
        return Ok(data.config.face_value);
    }

    // PARITY: Solidity computes `maturityDate - epochEndTime` and `now - epochEndTime`
    // directly, so a malformed config or a query before funding closes reverts on 0.8
    // underflow. Saturating here turns both into "nothing accrued yet". Identical for
    // every timestamp at or after epoch_end_time on a well-formed pool, which is every
    // call the protocol actually makes — but the indexer does query early, and a revert
    // there is noise, not signal.
    let total_time = data
        .config
        .maturity_date
        .saturating_sub(data.config.epoch_end_time);
    if total_time == 0 {
        return Ok(base);
    }

    let elapsed = now.saturating_sub(data.config.epoch_end_time);
    let capped = if elapsed > total_time { total_time } else { elapsed };
    let accrued = math::mul_div(
        data.total_discount_earned,
        capped as Amount,
        total_time as Amount,
    )?;
    math::checked_add(base, accrued)
}

/// Face value implied by an amount raised at a given discount rate.
pub fn face_value_from(actual_raised: Amount, discount_rate: u32) -> Result<Amount, Error> {
    if discount_rate >= BASIS_POINTS {
        return Err(Error::InvalidDiscountRate);
    }
    math::mul_div(
        actual_raised,
        BASIS_POINTS as Amount,
        (BASIS_POINTS - discount_rate) as Amount,
    )
}

/// Everything the pool pays out at maturity.
pub fn total_returns(data: &PoolData) -> Result<Amount, Error> {
    if data.is_discounted() {
        return Ok(data.config.face_value);
    }
    let undistributed = math::checked_sub(
        data.total_coupons_received,
        data.total_coupons_distributed,
    )?;
    math::checked_add(data.funds_returned_by_spv, undistributed)
}

/// Sum of the scheduled coupons, on invested principal where known and on the target
/// raise before investment.
pub fn expected_coupons(data: &PoolData) -> Result<Amount, Error> {
    if data.config.coupons.is_empty() {
        return Ok(0);
    }
    let principal = if data.actual_invested > 0 {
        data.actual_invested
    } else {
        data.config.target_raise
    };

    let mut total: Amount = 0;
    for (_, rate) in data.config.coupons.iter() {
        total = math::checked_add(total, math::bps_of(principal, rate)?)?;
    }
    Ok(total)
}

/// Yield the pool is expected to produce over its life, excluding principal.
pub fn expected_return(data: &PoolData) -> Result<Amount, Error> {
    if !data.is_discounted() {
        return expected_coupons(data);
    }
    if matches!(data.status, PoolStatus::Invested | PoolStatus::Matured) {
        return math::checked_sub(data.config.face_value, data.actual_invested);
    }
    let estimated = face_value_from(data.config.target_raise, data.config.discount_rate)?;
    math::checked_sub(estimated, data.config.target_raise)
}

/// What a holder of `user_shares` is worth at this moment.
///
/// During funding, shares are struck 1:1 with deposited assets, so the holding is worth
/// its face. After that it is a pro-rata claim on whatever the pool is currently worth.
pub fn user_return(
    data: &PoolData,
    user_shares: Amount,
    total_shares: Amount,
    now: u64,
) -> Result<Amount, Error> {
    if user_shares == 0 || total_shares == 0 {
        return Ok(0);
    }
    match data.status {
        PoolStatus::Funding => Ok(user_shares),
        PoolStatus::Invested => {
            let value = current_pool_value(data, now)?;
            math::pro_rata(user_shares, value, total_shares)
        }
        PoolStatus::Matured => {
            let returns = total_returns(data)?;
            math::pro_rata(user_shares, returns, total_shares)
        }
        PoolStatus::Emergency => math::pro_rata(user_shares, data.total_raised, total_shares),
        _ => Ok(0),
    }
}

/// Coupons received but not yet marked distributed.
pub fn undistributed_coupons(data: &PoolData) -> Amount {
    if !data.is_interest_bearing() {
        return 0;
    }
    data.total_coupons_received
        .saturating_sub(data.total_coupons_distributed)
}

/// Coupons marked distributed but not yet claimed by holders.
pub fn unclaimed_coupons(data: &PoolData) -> Amount {
    if !data.is_interest_bearing() {
        return 0;
    }
    data.total_coupons_distributed
        .saturating_sub(data.total_coupons_claimed)
}

/// Amount that a distribution would release, or the reason it cannot run.
pub fn distributable_coupons(data: &PoolData, total_shares: Amount) -> Result<Amount, Error> {
    if !data.is_interest_bearing() {
        return Err(Error::NotInterestBearing);
    }
    if data.status != PoolStatus::Invested {
        return Err(Error::NotInvested);
    }
    if total_shares == 0 {
        return Err(Error::NoSharesOutstanding);
    }
    let undistributed = math::checked_sub(
        data.total_coupons_received,
        data.total_coupons_distributed,
    )?;
    if undistributed == 0 {
        return Err(Error::NoCouponsToDistribute);
    }
    Ok(undistributed)
}

/// Result of a coupon claim: what to pay out, and what to write back.
///
/// `new_claimed_total` is the holder's cumulative entitlement, not the payout — storing
/// the running total is what makes a claim idempotent against repeated calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CouponClaim {
    pub claimable: Amount,
    pub new_claimed_total: Amount,
}

/// Computes a holder's claim against distributed coupons.
pub fn claim_coupon(
    data: &PoolData,
    user_shares: Amount,
    total_shares: Amount,
    already_claimed: Amount,
) -> Result<CouponClaim, Error> {
    if !data.is_interest_bearing() {
        return Err(Error::NotInterestBearing);
    }
    if data.status != PoolStatus::Invested {
        return Err(Error::NotInvested);
    }
    if user_shares == 0 {
        return Err(Error::NoShares);
    }
    // PARITY: the Solidity divides by totalSupply without a zero check, relying on it
    // being at least the caller's balance. Explicit here; unreachable when shares > 0.
    if total_shares == 0 {
        return Err(Error::NoSharesOutstanding);
    }
    if data.total_coupons_distributed == 0 {
        return Err(Error::NoCouponsDistributed);
    }

    let entitlement = math::pro_rata(user_shares, data.total_coupons_distributed, total_shares)?;
    if entitlement <= already_claimed {
        return Err(Error::NoNewCoupons);
    }

    Ok(CouponClaim {
        claimable: math::checked_sub(entitlement, already_claimed)?,
        new_claimed_total: entitlement,
    })
}

/// Read-only form of [`claim_coupon`] that reports zero instead of an error.
pub fn available_coupon(
    data: &PoolData,
    user_shares: Amount,
    total_shares: Amount,
    already_claimed: Amount,
) -> Amount {
    match claim_coupon(data, user_shares, total_shares, already_claimed) {
        Ok(claim) => claim.claimable,
        Err(_) => 0,
    }
}

/// Whether `now` falls within the tolerance window of any scheduled coupon date.
pub fn is_valid_coupon_date(config: &PoolConfig, now: u64) -> bool {
    config.coupons.dates().iter().any(|date| {
        // PARITY: Solidity computes `couponDate - tolerance`, which reverts on underflow
        // for any coupon date inside the first day of the unix epoch. Saturating makes
        // that a plain "outside the window" rather than a revert.
        let lower = date.saturating_sub(COUPON_DATE_TOLERANCE_SECS);
        let upper = date.saturating_add(COUPON_DATE_TOLERANCE_SECS);
        now >= lower && now <= upper
    })
}
