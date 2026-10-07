//! Net asset value for Stable Yield pools.
//!
//! Ported from `src/libraries/StableYieldNAVLibrary.sol`.
//!
//! The pool is priced by what it holds: instruments marked to model, plus whatever cash
//! sits in escrow. Shares are struck against that, so a deposit and a withdrawal on the
//! same day get the same price.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::stable_yield::types::{InstrumentHolding, InstrumentType};
use crate::BASIS_POINTS;

pub const SECONDS_PER_YEAR: u64 = 365 * 86_400;

/// Fixed-point scale for NAV per share, matching the Solidity's `1e18`.
pub const NAV_PRECISION: Amount = 1_000_000_000_000_000_000;

/// Mark-to-model value of a discounted instrument.
///
/// Accretes linearly from purchase price to face value, then sits at face.
pub fn discounted_value(instrument: &InstrumentHolding, now: u64) -> Result<Amount, Error> {
    // PARITY: the Solidity subtracts directly and reverts on 0.8 underflow if valued
    // before purchase or with a malformed date pair. Saturating reads as "not started".
    let total_time = instrument
        .maturity_date
        .saturating_sub(instrument.purchase_date);
    let elapsed = now.saturating_sub(instrument.purchase_date);

    if elapsed >= total_time {
        return Ok(instrument.face_value);
    }

    let spread = math::checked_sub(instrument.face_value, instrument.purchase_price)?;
    let accrued = math::mul_div(spread, elapsed as Amount, total_time as Amount)?;
    math::checked_add(instrument.purchase_price, accrued)
}

/// Mark-to-model value of an interest-bearing instrument: the capital accreted from what
/// was paid toward face, plus the coupon accrued since the last payment.
///
/// Marking straight at face ignored `purchase_price`, so an instrument bought below par
/// booked the whole discount the moment it was recorded — an SPV could lift NAV simply by
/// buying cheaply, and the gain was unearned until the bond actually redeemed. A default
/// before maturity would then have been a fall from a level the pool never truly held.
/// The discount now accretes over the holding period, the same way a discounted
/// instrument's does.
pub fn interest_bearing_value(instrument: &InstrumentHolding, now: u64) -> Result<Amount, Error> {
    if instrument.coupon_frequency == 0 {
        return Err(Error::InvalidCouponFrequency);
    }

    // Interest stops accruing at maturity. Without this an instrument the SPV never
    // settles keeps inflating NAV without bound, and holders redeem against value the
    // pool will never receive.
    let now = now.min(instrument.maturity_date);

    let coupon_period = SECONDS_PER_YEAR / instrument.coupon_frequency as u64;
    if coupon_period == 0 {
        return Err(Error::InvalidCouponFrequency);
    }

    let last_coupon_date = if instrument.coupons_paid == 0 {
        instrument.purchase_date
    } else {
        instrument.next_coupon_due_date.saturating_sub(coupon_period)
    };

    // Capped at one period. Beyond the due date the coupon is late, not larger: without
    // this a payment the SPV never makes goes on inflating NAV until maturity, and
    // holders redeem against income that never arrived.
    let since_last = now.saturating_sub(last_coupon_date).min(coupon_period);
    let denominator = (BASIS_POINTS as Amount)
        .checked_mul(instrument.coupon_frequency as Amount)
        .ok_or(Error::Overflow)?;
    let coupon_amount = math::mul_div(
        instrument.face_value,
        instrument.annual_coupon_rate as Amount,
        denominator,
    )?;
    let accrued = math::mul_div(coupon_amount, since_last as Amount, coupon_period as Amount)?;

    // The capital leg accretes purchase price toward face over the holding period, and is
    // capped at face once the term is up — exactly `discounted_value`, so the two
    // instrument types agree about what holding something below par is worth.
    let capital = discounted_value(instrument, now)?;

    math::checked_add(capital, accrued)
}

/// Value of one holding, whichever kind it is.
pub fn instrument_value(instrument: &InstrumentHolding, now: u64) -> Result<Amount, Error> {
    match instrument.instrument_type {
        InstrumentType::Discounted => discounted_value(instrument, now),
        InstrumentType::InterestBearing => interest_bearing_value(instrument, now),
    }
}

/// Sums the active holdings in a slice.
///
/// Takes a slice rather than reaching for storage, so a binding can feed it one page at a
/// time and stay inside a Soroban footprint or a Solana account list. Inactive holdings
/// contribute nothing.
pub fn gross_asset_value(
    instruments: &[InstrumentHolding],
    now: u64,
) -> Result<Amount, Error> {
    let mut total: Amount = 0;
    for instrument in instruments {
        if instrument.is_active {
            total = math::checked_add(total, instrument_value(instrument, now)?)?;
        }
    }
    Ok(total)
}

/// Everything the pool is worth: instruments plus uninvested cash.
pub fn pool_nav(gross_asset_value: Amount, pool_reserves: Amount) -> Result<Amount, Error> {
    math::checked_add(gross_asset_value, pool_reserves)
}

/// Price of one share, scaled by [`NAV_PRECISION`].
///
/// An empty pool prices at exactly one, so the first depositor is not handed the whole
/// pool for a rounding error.
pub fn nav_per_share(total_nav: Amount, total_shares: Amount) -> Result<Amount, Error> {
    if total_shares == 0 {
        return Ok(NAV_PRECISION);
    }
    math::mul_div(total_nav, NAV_PRECISION, total_shares)
}

/// Shares minted for a deposit at the given price.
pub fn shares_for_deposit(assets: Amount, nav_per_share: Amount) -> Result<Amount, Error> {
    if nav_per_share <= 0 {
        return Err(Error::InvalidNavPerShare);
    }
    math::mul_div(assets, NAV_PRECISION, nav_per_share)
}

/// Assets owed for shares at the given price.
pub fn assets_for_shares(shares: Amount, nav_per_share: Amount) -> Result<Amount, Error> {
    if nav_per_share <= 0 {
        return Err(Error::InvalidNavPerShare);
    }
    math::mul_div(shares, nav_per_share, NAV_PRECISION)
}
