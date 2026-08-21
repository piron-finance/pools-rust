//! Pool state machine.
//!
//! Ported from `src/libraries/PoolLifecycleLibrary.sol`.
//!
//! ```text
//!   FUNDING ──filled──> FILLED ─┐
//!      │                        ├──close_epoch──> PENDING_INVESTMENT ──invest──> INVESTED
//!      └────────────────────────┘         │                                          │
//!                                    underfunded                                  matured
//!                                         v                                          v
//!                                     EMERGENCY                                  MATURED
//!                                                                                    │
//!                                                                          all shares redeemed
//!                                                                                    v
//!                                                                                WITHDRAWN
//! ```
//!
//! Every function here is pure: it takes the current state and returns the values to
//! write. Nothing mutates, nothing reads storage, nothing moves tokens. The binding
//! applies the outcome and emits the event.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::single_asset::calc;
use crate::types::{PoolConfig, PoolData, PoolStatus};
use crate::BASIS_POINTS;

/// Capital that must be raised for the deal to proceed, from the threshold in bps.
pub fn minimum_funding_required(config: &PoolConfig) -> Result<Amount, Error> {
    math::bps_of(config.target_raise, config.minimum_funding_threshold)
}

/// `FUNDING -> FILLED`, once the target raise is met.
pub fn mark_filled(data: &PoolData) -> Result<PoolStatus, Error> {
    if data.status != PoolStatus::Funding {
        return Err(Error::NotFundingPhase);
    }
    if data.total_raised < data.config.target_raise {
        return Err(Error::TargetNotReached);
    }
    Ok(PoolStatus::Filled)
}

/// What closing the funding epoch produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochClose {
    pub new_status: PoolStatus,
    /// Face value implied by the amount actually raised. Zero for interest-bearing pools
    /// and for an underfunded epoch.
    pub face_value: Amount,
    /// Set when the raise fell short of the minimum threshold.
    pub underfunded: bool,
}

/// `{FUNDING, FILLED} -> PENDING_INVESTMENT`, or `-> EMERGENCY` if underfunded.
///
/// `force` is the admin path (`forceCloseEpoch`): it skips the epoch-end check but, as in
/// the Solidity, only applies to a pool still in `FUNDING`.
pub fn close_epoch(data: &PoolData, now: u64, force: bool) -> Result<EpochClose, Error> {
    if force {
        if data.status != PoolStatus::Funding {
            return Err(Error::NotFundingPhase);
        }
    } else {
        if !matches!(data.status, PoolStatus::Funding | PoolStatus::Filled) {
            return Err(Error::NotFundingPhase);
        }
        // A FILLED pool has already hit its target, so it closes without waiting.
        if data.status == PoolStatus::Funding && now < data.config.epoch_end_time {
            return Err(Error::EpochNotEnded);
        }
    }

    let raised = data.total_raised;
    let minimum = minimum_funding_required(&data.config)?;

    if raised < minimum {
        return Ok(EpochClose {
            new_status: PoolStatus::Emergency,
            face_value: 0,
            underfunded: true,
        });
    }

    let face_value = if data.is_discounted() {
        calc::face_value_from(raised, data.config.discount_rate)?
    } else {
        0
    };

    Ok(EpochClose {
        new_status: PoolStatus::PendingInvestment,
        face_value,
        underfunded: false,
    })
}

/// Preconditions for releasing escrow funds to the SPV.
pub fn validate_spv_withdrawal(data: &PoolData, amount: Amount) -> Result<(), Error> {
    if data.status != PoolStatus::PendingInvestment {
        return Err(Error::NotPendingInvestment);
    }
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

/// What confirming the SPV's investment produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvestmentOutcome {
    pub new_status: PoolStatus,
    pub actual_invested: Amount,
    pub face_value: Amount,
    pub total_discount_earned: Amount,
}

/// `PENDING_INVESTMENT -> INVESTED`.
///
/// Face value and the discount to be accrued are both fixed here, from the amount the SPV
/// actually deployed rather than the amount raised — the two differ whenever the SPV buys
/// at a different size than the pool collected.
pub fn process_investment(
    data: &PoolData,
    actual_amount: Amount,
    now: u64,
) -> Result<InvestmentOutcome, Error> {
    if data.status != PoolStatus::PendingInvestment {
        return Err(Error::NotPendingInvestment);
    }
    if actual_amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    if actual_amount > data.total_raised {
        return Err(Error::InvestmentExceedsRaised);
    }

    if data.is_discounted() {
        let face_value = calc::face_value_from(actual_amount, data.config.discount_rate)?;
        return Ok(InvestmentOutcome {
            new_status: PoolStatus::Invested,
            actual_invested: actual_amount,
            face_value,
            total_discount_earned: math::checked_sub(face_value, actual_amount)?,
        });
    }

    // The dates/rates length invariant is guaranteed by `CouponSchedule`, so only the
    // "first coupon still payable" check remains from the Solidity's three.
    if let Some(first) = data.config.coupons.first_date() {
        if first <= now {
            return Err(Error::CouponDateInPast);
        }
    }

    Ok(InvestmentOutcome {
        new_status: PoolStatus::Invested,
        actual_invested: actual_amount,
        face_value: 0,
        total_discount_earned: 0,
    })
}

/// What the pool was expected to settle at, principal included.
///
/// PARITY: this is `PoolLifecycleLibrary._calculateExpectedReturn`, which is a *different*
/// quantity from `CalculationLibrary.calculateExpectedReturn` (ported as
/// [`calc::expected_return`]) despite the near-identical name. That one is yield only;
/// this one is the whole settlement and is used solely to classify the SPV's return.
pub fn expected_settlement(data: &PoolData) -> Amount {
    if data.is_discounted() {
        data.config.face_value
    } else {
        data.actual_invested
    }
}

/// How the SPV's actual return compared to what was expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaturityVariance {
    /// At or above expectation, within 120%.
    OnTarget,
    /// Returned less than expected, by this much.
    Shortfall(Amount),
    /// Returned more than 120% of expectation — flagged for review, not rejected.
    Overage,
}

/// What settling at maturity produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaturityOutcome {
    pub new_status: PoolStatus,
    pub funds_returned: Amount,
    pub variance: MaturityVariance,
}

/// `INVESTED -> MATURED`.
pub fn process_maturity(
    data: &PoolData,
    final_amount: Amount,
    now: u64,
) -> Result<MaturityOutcome, Error> {
    super::validate::validate_maturity_processing(data, final_amount, now)?;

    let expected = expected_settlement(data);
    let variance = if final_amount < expected {
        MaturityVariance::Shortfall(math::checked_sub(expected, final_amount)?)
    } else if final_amount > math::mul_div(expected, 120, 100)? {
        MaturityVariance::Overage
    } else {
        MaturityVariance::OnTarget
    };

    Ok(MaturityOutcome {
        new_status: PoolStatus::Matured,
        funds_returned: math::checked_add(data.funds_returned_by_spv, final_amount)?,
        variance,
    })
}

/// `MATURED -> WITHDRAWN`, once no shares remain outstanding.
pub fn mark_withdrawn(data: &PoolData, total_shares: Amount) -> Result<PoolStatus, Error> {
    if data.status != PoolStatus::Matured {
        return Err(Error::NotMatured);
    }
    if total_shares != 0 {
        return Err(Error::SharesStillOutstanding);
    }
    Ok(PoolStatus::Withdrawn)
}

/// Moves a pool to `EMERGENCY` from any live state.
///
/// Deliberately permissive about the starting status: emergency is the escape hatch, and
/// refusing to enter it because of a state check is the wrong failure mode. Only a pool
/// that is already finished cannot enter it.
pub fn declare_emergency(data: &PoolData) -> Result<PoolStatus, Error> {
    if matches!(data.status, PoolStatus::Withdrawn | PoolStatus::Emergency) {
        return Err(Error::InvalidStatusForOperation);
    }
    Ok(PoolStatus::Emergency)
}

/// Convenience for bindings: is this status one where capital is still recoverable
/// without SPV involvement?
pub fn is_refundable(status: PoolStatus) -> bool {
    matches!(status, PoolStatus::Funding | PoolStatus::Emergency)
}

const _: () = assert!(BASIS_POINTS == 10_000);
