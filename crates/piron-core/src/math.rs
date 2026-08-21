//! Checked arithmetic for protocol amounts.
//!
//! Every amount is `i128` — Soroban's native integer. Nothing here wraps and nothing
//! panics; overflow is an error the caller handles.

use crate::error::Error;
use crate::BASIS_POINTS;

/// A protocol amount in an asset's smallest unit. Always non-negative in practice;
/// the type is signed because that is what the host offers.
pub type Amount = i128;

/// Rejects negative amounts at the boundary so the rest of the module can assume
/// non-negative operands.
pub fn require_non_negative(a: Amount) -> Result<Amount, Error> {
    if a < 0 {
        return Err(Error::NegativeAmount);
    }
    Ok(a)
}

pub fn checked_add(a: Amount, b: Amount) -> Result<Amount, Error> {
    a.checked_add(b).ok_or(Error::Overflow)
}

pub fn checked_sub(a: Amount, b: Amount) -> Result<Amount, Error> {
    a.checked_sub(b).ok_or(Error::Overflow)
}

pub fn checked_mul(a: Amount, b: Amount) -> Result<Amount, Error> {
    a.checked_mul(b).ok_or(Error::Overflow)
}

/// `(a * b) / denom`, truncating toward zero, without overflowing on the intermediate
/// product where it can be avoided.
///
/// The fast path is a plain 128-bit multiply. When that would overflow, the operands are
/// split as `a = q*denom + r`, giving `q*b + (r*b)/denom`, which is exactly equal to
/// `floor(a*b/denom)` for non-negative inputs and keeps both partial products smaller.
///
/// Truncation direction matches Solidity's integer division, so pro-rata dust is left in
/// the escrow on both chains rather than over-distributed on one.
pub fn mul_div(a: Amount, b: Amount, denom: Amount) -> Result<Amount, Error> {
    require_non_negative(a)?;
    require_non_negative(b)?;
    require_non_negative(denom)?;
    if denom == 0 {
        return Err(Error::DivisionByZero);
    }

    if let Some(product) = a.checked_mul(b) {
        return Ok(product / denom);
    }

    let quotient = a / denom;
    let remainder = a % denom;
    let high = quotient.checked_mul(b).ok_or(Error::Overflow)?;
    let low = remainder.checked_mul(b).ok_or(Error::Overflow)? / denom;
    checked_add(high, low)
}

/// `amount * bps / 10_000`.
pub fn bps_of(amount: Amount, bps: u32) -> Result<Amount, Error> {
    mul_div(amount, bps as Amount, BASIS_POINTS as Amount)
}

/// Pro-rata share of `total` owed to a holder of `part` out of `whole`.
///
/// Returns zero when the holder has nothing or when there is nothing outstanding —
/// callers that need those cases to be errors check first.
pub fn pro_rata(part: Amount, total: Amount, whole: Amount) -> Result<Amount, Error> {
    if part == 0 || whole == 0 {
        return Ok(0);
    }
    mul_div(part, total, whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_div_truncates_toward_zero() {
        assert_eq!(mul_div(7, 3, 2).unwrap(), 10); // 21/2 = 10.5 -> 10
        assert_eq!(mul_div(1, 1, 3).unwrap(), 0);
    }

    #[test]
    fn mul_div_rejects_zero_denominator() {
        assert_eq!(mul_div(1, 1, 0), Err(Error::DivisionByZero));
    }

    #[test]
    fn mul_div_rejects_negative_operands() {
        assert_eq!(mul_div(-1, 1, 1), Err(Error::NegativeAmount));
        assert_eq!(mul_div(1, -1, 1), Err(Error::NegativeAmount));
    }

    #[test]
    fn mul_div_survives_an_overflowing_intermediate() {
        // a*b overflows i128, but the result is representable.
        let a = i128::MAX / 2;
        let b = 4;
        let denom = 8;
        assert_eq!(mul_div(a, b, denom).unwrap(), a / 2);
    }

    #[test]
    fn mul_div_matches_direct_computation_when_it_fits() {
        let a: Amount = 1_000_000_000_000_000;
        let b: Amount = 3;
        let denom: Amount = 7;
        let direct = (a * b) / denom;
        assert_eq!(mul_div(a, b, denom).unwrap(), direct);
    }

    #[test]
    fn the_split_path_agrees_with_the_fast_path() {
        // Forced through the split: a*b overflows, but a/2 is representable.
        let a = i128::MAX / 3;
        assert_eq!(mul_div(a, 6, 12).unwrap(), a / 2);
    }

    #[test]
    fn realistic_pool_scale_does_not_overflow() {
        // $100M pool at 7 decimals, holder with 1% of supply.
        let supply: Amount = 1_000_000_000_000_000;
        let value: Amount = 1_050_000_000_000_000;
        let shares: Amount = supply / 100;
        assert_eq!(pro_rata(shares, value, supply).unwrap(), value / 100);
    }

    #[test]
    fn bps_of_matches_basis_point_semantics() {
        assert_eq!(bps_of(10_000, 250).unwrap(), 250); // 2.5%
        assert_eq!(bps_of(1_000_000, 10_000).unwrap(), 1_000_000); // 100%
        assert_eq!(bps_of(999, 1).unwrap(), 0); // truncates
    }

    #[test]
    fn pro_rata_is_zero_for_empty_positions() {
        assert_eq!(pro_rata(0, 100, 10).unwrap(), 0);
        assert_eq!(pro_rata(10, 100, 0).unwrap(), 0);
    }
}
