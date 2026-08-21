//! Fee accounting.
//!
//! Ported from `src/FeeManager.sol`.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::BASIS_POINTS;

/// What a fee was charged for. Ordering is the wire encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FeeType {
    Deposit = 0,
    Withdrawal = 1,
    EarlyExitPenalty = 2,
    Spread = 3,
    ExcessYield = 4,
    Performance = 5,
    Other = 6,
}

impl FeeType {
    pub const COUNT: u32 = 7;

    pub fn from_u32(value: u32) -> Result<Self, Error> {
        Ok(match value {
            0 => FeeType::Deposit,
            1 => FeeType::Withdrawal,
            2 => FeeType::EarlyExitPenalty,
            3 => FeeType::Spread,
            4 => FeeType::ExcessYield,
            5 => FeeType::Performance,
            6 => FeeType::Other,
            _ => return Err(Error::UnknownFeeType),
        })
    }
}

/// How one fee type is divided. The three shares must total exactly 100%.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSplit {
    pub treasury_bps: u32,
    pub reserve_bps: u32,
    pub ops_bps: u32,
    pub active: bool,
}

impl FeeSplit {
    /// A split that sends everything to the treasury. Sensible only as a placeholder.
    pub const fn all_treasury() -> Self {
        Self {
            treasury_bps: BASIS_POINTS,
            reserve_bps: 0,
            ops_bps: 0,
            active: true,
        }
    }
}

/// One fee, divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeAllocation {
    pub to_treasury: Amount,
    pub to_reserve: Amount,
    pub to_ops: Amount,
}

impl FeeAllocation {
    pub fn total(&self) -> Result<Amount, Error> {
        math::checked_add(
            math::checked_add(self.to_treasury, self.to_reserve)?,
            self.to_ops,
        )
    }
}

/// Checks a split is usable: active, and summing to exactly 100%.
pub fn validate_split(split: &FeeSplit) -> Result<(), Error> {
    if !split.active {
        return Err(Error::FeeTypeInactive);
    }
    let total = split
        .treasury_bps
        .checked_add(split.reserve_bps)
        .and_then(|v| v.checked_add(split.ops_bps))
        .ok_or(Error::Overflow)?;
    if total != BASIS_POINTS {
        return Err(Error::InvalidSplitRatio);
    }
    Ok(())
}

/// Divides `amount` three ways.
///
/// Ops takes the remainder rather than its own rounded share, so the three parts always
/// sum back to exactly `amount` — no dust is created or lost, on any runtime.
pub fn split_fee(amount: Amount, split: &FeeSplit) -> Result<FeeAllocation, Error> {
    validate_split(split)?;
    if amount < 0 {
        return Err(Error::NegativeAmount);
    }

    let to_treasury = math::bps_of(amount, split.treasury_bps)?;
    let to_reserve = math::bps_of(amount, split.reserve_bps)?;
    let to_ops = math::checked_sub(math::checked_sub(amount, to_treasury)?, to_reserve)?;

    Ok(FeeAllocation {
        to_treasury,
        to_reserve,
        to_ops,
    })
}
