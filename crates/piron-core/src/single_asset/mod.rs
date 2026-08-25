//! Single-Asset (deal) pools: fixed-term, single-instrument.
//!
//! Ported from `src/libraries/CalculationLibrary.sol` and
//! `src/libraries/ValidationLibrary.sol`.
//!
//! Nothing here reads storage or a token balance. Share balances, total supply and the
//! current time are arguments, so every rule in this module is testable without a chain
//! and portable to any runtime that can call a Rust function.

pub mod calc;
pub mod flow;
pub mod lifecycle;
pub mod validate;

pub use calc::{
    available_coupon, claim_coupon, current_pool_value, distributable_coupons, expected_coupons,
    expected_return, face_value_from, is_valid_coupon_date, total_returns, undistributed_coupons,
    unclaimed_coupons, user_return, CouponClaim, COUPON_DATE_TOLERANCE_SECS,
};
pub use flow::{
    deposit, withdraw, DepositOutcome, WithdrawRequest, WithdrawalKind,
    WithdrawalOutcome,
};
pub use lifecycle::{
    close_epoch, declare_emergency, expected_settlement, mark_filled, mark_withdrawn,
    minimum_funding_required, process_investment, process_maturity, validate_spv_withdrawal,
    EpochClose, InvestmentOutcome, MaturityOutcome, MaturityVariance,
};
pub use validate::{
    validate_config, validate_coupon_payment, validate_deposit, validate_discount_distribution,
    validate_emergency_refunds, validate_maturity_processing, validate_pool_cancellation,
    validate_withdrawal,
};
