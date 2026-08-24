//! Preconditions for Single-Asset pool operations.
//!
//! Ported from `src/libraries/ValidationLibrary.sol`.
//!
//! PARITY: the Solidity versions additionally check `poolRegistry.isRegisteredPool`,
//! `poolRegistry.isApprovedAsset` and non-zero addresses. Those are cross-contract reads
//! and address checks, so they stay in the contract layer where the registry client and
//! the `Address` type live. Every rule that is a *decision about protocol state* is here.

use crate::error::Error;
use crate::math::{self, Amount};
use crate::types::{PoolConfig, PoolData, PoolStatus};
use crate::{BASIS_POINTS, MAX_COUPON_SCHEDULE};

/// Deal terms, checked once before a pool opens for funding.
///
/// The bounds here are protocol rules on every runtime, not Solana rules — but two of
/// them exist because Solana accounts are sized at creation and cannot hold an unbounded
/// schedule. Enforcing them everywhere keeps a pool that is valid on one chain valid on
/// all of them.
pub fn validate_config(config: &PoolConfig) -> Result<(), Error> {
    if config.discount_rate >= BASIS_POINTS {
        return Err(Error::InvalidDiscountRate);
    }
    if config.withdrawal_fee_bps > BASIS_POINTS {
        return Err(Error::InvalidBps);
    }
    if config.minimum_funding_threshold == 0 || config.minimum_funding_threshold > BASIS_POINTS {
        return Err(Error::InvalidBps);
    }
    // Length and pairing are enforced by `CouponSchedule`'s constructor; re-checked here
    // so a hand-built config cannot slip past.
    if config.coupons.len() > MAX_COUPON_SCHEDULE {
        return Err(Error::CouponScheduleTooLong);
    }
    if config.maturity_date <= config.epoch_end_time {
        return Err(Error::InvalidMaturityDate);
    }
    if config.target_raise <= 0 || config.min_investment <= 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

/// A deposit during the funding window.
pub fn validate_deposit(data: &PoolData, assets: Amount, now: u64) -> Result<(), Error> {
    if assets <= 0 {
        return Err(Error::InvalidAmount);
    }
    if assets < data.config.min_investment {
        return Err(Error::BelowMinInvestment);
    }
    if data.status != PoolStatus::Funding {
        return Err(Error::NotFundingPhase);
    }
    if now > data.config.epoch_end_time {
        return Err(Error::FundingEnded);
    }
    let raised_after = math::checked_add(data.total_raised, assets)?;
    if raised_after > data.config.target_raise {
        return Err(Error::ExceedsTargetRaise);
    }
    Ok(())
}

/// Whether the pool's current state permits any withdrawal at all.
///
/// Withdrawing during funding is a cancellation, at maturity a redemption, and in
/// emergency a refund — three different flows, one gate.
pub fn validate_withdrawal(data: &PoolData, _now: u64) -> Result<(), Error> {
    // INVESTED is deliberately absent. The gate used to admit it once past the maturity
    // date, but the router has no INVESTED branch and rejects it regardless, so the only
    // thing the extra clause produced was a misleading error at a later line. Settlement
    // moves the pool to MATURED; that is what opens redemption.
    let allowed = matches!(
        data.status,
        PoolStatus::Funding | PoolStatus::Matured | PoolStatus::Emergency
    );

    if !allowed {
        return Err(Error::WithdrawalsNotAllowed);
    }
    Ok(())
}

/// Settling the deal at maturity with the amount the SPV returned.
pub fn validate_maturity_processing(
    data: &PoolData,
    final_amount: Amount,
    now: u64,
) -> Result<(), Error> {
    if data.status != PoolStatus::Invested {
        return Err(Error::NotInvested);
    }
    if now < data.config.maturity_date {
        return Err(Error::MaturityNotReached);
    }
    if final_amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

/// Paying out the accrued discount on a matured discounted instrument.
pub fn validate_discount_distribution(data: &PoolData) -> Result<(), Error> {
    if data.status != PoolStatus::Matured {
        return Err(Error::NotMatured);
    }
    if !data.is_discounted() {
        return Err(Error::NotDiscounted);
    }
    Ok(())
}

/// Recording an incoming coupon from the SPV.
pub fn validate_coupon_payment(data: &PoolData, amount: Amount) -> Result<(), Error> {
    if data.status != PoolStatus::Invested {
        return Err(Error::NotInvested);
    }
    if !data.is_interest_bearing() {
        return Err(Error::NotInterestBearing);
    }
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }
    Ok(())
}

pub fn validate_emergency_refunds(data: &PoolData) -> Result<(), Error> {
    if data.status != PoolStatus::Emergency {
        return Err(Error::NotInEmergency);
    }
    Ok(())
}

pub fn validate_pool_cancellation(data: &PoolData) -> Result<(), Error> {
    if data.status != PoolStatus::Funding {
        return Err(Error::NotFundingPhase);
    }
    Ok(())
}
