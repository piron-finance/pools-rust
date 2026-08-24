//! Behavioural tests for Single-Asset pools.
//!
//! Scenario names mirror the Foundry tests in `test/SingleAssetPool.t.sol` so the two
//! suites can be diffed by eye until conformance vectors replace the eyeballing.

use piron_core::error::Error;
use piron_core::math::Amount;
use piron_core::single_asset::{calc, validate};
use piron_core::schedule::CouponSchedule;
use piron_core::types::{InstrumentType, PoolConfig, PoolData, PoolStatus};

const EPOCH_END: u64 = 1_000;
const MATURITY: u64 = 11_000;
const TARGET_RAISE: Amount = 1_000_000;

fn discounted_config() -> PoolConfig {
    PoolConfig {
        instrument_type: InstrumentType::Discounted,
        face_value: 1_100_000,
        purchase_price: 1_000_000,
        target_raise: TARGET_RAISE,
        epoch_end_time: EPOCH_END,
        maturity_date: MATURITY,
        coupons: CouponSchedule::empty(),
        discount_rate: 500,
        minimum_funding_threshold: 8_000,
        min_investment: 1_000,
        withdrawal_fee_bps: 0,
    }
}

fn interest_bearing_config() -> PoolConfig {
    PoolConfig {
        instrument_type: InstrumentType::InterestBearing,
        face_value: 0,
        purchase_price: 1_000_000,
        target_raise: TARGET_RAISE,
        epoch_end_time: EPOCH_END,
        maturity_date: MATURITY,
        coupons: CouponSchedule::from_parts(&[5_000, 9_000], &[250, 250]).unwrap(),
        discount_rate: 0,
        minimum_funding_threshold: 8_000,
        min_investment: 1_000,
        withdrawal_fee_bps: 0,
    }
}

/// A discounted pool that has been funded and deployed to the SPV.
fn invested_discounted() -> PoolData {
    let mut data = PoolData::new(discounted_config());
    data.status = PoolStatus::Invested;
    data.total_raised = 1_000_000;
    data.actual_invested = 1_000_000;
    data.total_discount_earned = 100_000;
    data
}

/// An interest-bearing pool mid-life, with one coupon received and distributed.
fn invested_interest_bearing() -> PoolData {
    let mut data = PoolData::new(interest_bearing_config());
    data.status = PoolStatus::Invested;
    data.total_raised = 1_000_000;
    data.actual_invested = 1_000_000;
    data.total_coupons_received = 25_000;
    data.total_coupons_distributed = 25_000;
    data
}

// ---------------------------------------------------------------- valuation

#[test]
fn discount_accrues_linearly_across_the_term() {
    let data = invested_discounted();
    // Halfway between funding close and maturity: half the discount earned.
    assert_eq!(calc::current_pool_value(&data, 6_000).unwrap(), 1_050_000);
    // A quarter of the way.
    assert_eq!(calc::current_pool_value(&data, 3_500).unwrap(), 1_025_000);
}

#[test]
fn discounted_value_is_face_value_from_maturity_onward() {
    let data = invested_discounted();
    assert_eq!(calc::current_pool_value(&data, MATURITY).unwrap(), 1_100_000);
    assert_eq!(calc::current_pool_value(&data, 99_999).unwrap(), 1_100_000);
}

#[test]
fn discounted_value_before_funding_closes_has_accrued_nothing() {
    // PARITY: the Solidity reverts on this query. See calc::current_pool_value.
    let data = invested_discounted();
    assert_eq!(calc::current_pool_value(&data, 500).unwrap(), 1_000_000);
}

#[test]
fn interest_bearing_value_is_principal_plus_coupons_received() {
    let data = invested_interest_bearing();
    assert_eq!(calc::current_pool_value(&data, 6_000).unwrap(), 1_025_000);
}

#[test]
fn face_value_is_derived_from_the_discount_rate() {
    // 1,000,000 raised at a 5% discount accretes to 1,052,631 (truncated).
    assert_eq!(calc::face_value_from(1_000_000, 500).unwrap(), 1_052_631);
}

#[test]
fn a_hundred_percent_discount_rate_is_rejected() {
    assert_eq!(
        calc::face_value_from(1_000_000, 10_000),
        Err(Error::InvalidDiscountRate)
    );
}

// ---------------------------------------------------------------- returns

#[test]
fn expected_coupons_sum_the_schedule_on_invested_principal() {
    let data = invested_interest_bearing();
    // Two coupons at 2.5% of 1,000,000.
    assert_eq!(calc::expected_coupons(&data).unwrap(), 50_000);
}

#[test]
fn expected_coupons_fall_back_to_target_raise_before_investment() {
    let data = PoolData::new(interest_bearing_config());
    assert_eq!(data.actual_invested, 0);
    assert_eq!(calc::expected_coupons(&data).unwrap(), 50_000);
}

#[test]
fn expected_return_on_a_live_discounted_deal_is_face_less_invested() {
    let data = invested_discounted();
    assert_eq!(calc::expected_return(&data).unwrap(), 100_000);
}

#[test]
fn expected_return_before_investment_is_estimated_from_the_discount_rate() {
    let data = PoolData::new(discounted_config());
    // 1,052,631 implied face value less the 1,000,000 target.
    assert_eq!(calc::expected_return(&data).unwrap(), 52_631);
}

#[test]
fn total_returns_come_from_what_was_returned_whatever_the_instrument_kind() {
    // A discounted deal that has not settled yet has nothing to distribute — face value
    // is the target, not money in hand.
    assert_eq!(calc::total_returns(&invested_discounted()).unwrap(), 0);

    let mut settled = invested_discounted();
    settled.funds_returned_by_spv = 1_100_000;
    assert_eq!(calc::total_returns(&settled).unwrap(), 1_100_000);

    // A deal the SPV settled below face distributes the shortfall, not the projection.
    let mut short = invested_discounted();
    short.funds_returned_by_spv = 1_040_000;
    assert_eq!(calc::total_returns(&short).unwrap(), 1_040_000);

    let mut data = invested_interest_bearing();
    data.funds_returned_by_spv = 1_000_000;
    data.total_coupons_received = 50_000;
    data.total_coupons_distributed = 25_000;
    // Principal returned plus the coupon tranche not yet distributed.
    assert_eq!(calc::total_returns(&data).unwrap(), 1_025_000);
}

// ---------------------------------------------------------------- holdings

#[test]
fn shares_are_worth_their_deposit_during_funding() {
    let data = PoolData::new(discounted_config());
    assert_eq!(calc::user_return(&data, 100_000, 1_000_000, 500).unwrap(), 100_000);
}

#[test]
fn an_invested_holding_is_pro_rata_of_current_value() {
    let data = invested_discounted();
    // 10% of a pool worth 1,050,000 halfway through the term.
    assert_eq!(calc::user_return(&data, 100_000, 1_000_000, 6_000).unwrap(), 105_000);
}

#[test]
fn an_emergency_holding_is_pro_rata_of_the_amount_raised() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Emergency;
    assert_eq!(calc::user_return(&data, 100_000, 1_000_000, 6_000).unwrap(), 100_000);
}

#[test]
fn an_empty_holding_is_worth_nothing() {
    let data = invested_discounted();
    assert_eq!(calc::user_return(&data, 0, 1_000_000, 6_000).unwrap(), 0);
    assert_eq!(calc::user_return(&data, 100_000, 0, 6_000).unwrap(), 0);
}

// ---------------------------------------------------------------- coupons

#[test]
fn a_holder_claims_their_pro_rata_share_once() {
    let data = invested_interest_bearing();
    let claim = calc::claim_coupon(&data, 100_000, 1_000_000, 0).unwrap();
    assert_eq!(claim.claimable, 2_500);
    assert_eq!(claim.new_claimed_total, 2_500);
}

#[test]
fn claiming_twice_against_the_same_distribution_is_rejected() {
    let data = invested_interest_bearing();
    let first = calc::claim_coupon(&data, 100_000, 1_000_000, 0).unwrap();
    assert_eq!(
        calc::claim_coupon(&data, 100_000, 1_000_000, first.new_claimed_total),
        Err(Error::NoNewCoupons)
    );
}

#[test]
fn a_second_distribution_makes_the_difference_claimable() {
    let mut data = invested_interest_bearing();
    data.total_coupons_received = 50_000;
    data.total_coupons_distributed = 50_000;
    // Holder already took 2,500 from the first tranche.
    let claim = calc::claim_coupon(&data, 100_000, 1_000_000, 2_500).unwrap();
    assert_eq!(claim.claimable, 2_500);
    assert_eq!(claim.new_claimed_total, 5_000);
}

#[test]
fn coupon_claims_are_rejected_on_discounted_pools() {
    let data = invested_discounted();
    assert_eq!(
        calc::claim_coupon(&data, 100_000, 1_000_000, 0),
        Err(Error::NotInterestBearing)
    );
}

#[test]
fn the_view_reports_zero_where_a_claim_would_error() {
    let data = invested_discounted();
    assert_eq!(calc::available_coupon(&data, 100_000, 1_000_000, 0), 0);
}

#[test]
fn distribution_requires_undistributed_coupons() {
    let data = invested_interest_bearing();
    assert_eq!(
        calc::distributable_coupons(&data, 1_000_000),
        Err(Error::NoCouponsToDistribute)
    );

    let mut data = invested_interest_bearing();
    data.total_coupons_received = 50_000;
    assert_eq!(calc::distributable_coupons(&data, 1_000_000).unwrap(), 25_000);
}

#[test]
fn coupon_dates_are_valid_within_a_day_either_side() {
    // One date, so the window boundary is unambiguous.
    let mut config = interest_bearing_config();
    config.coupons = CouponSchedule::from_parts(&[1_000_000], &[250]).unwrap();

    assert!(calc::is_valid_coupon_date(&config, 1_000_000));
    assert!(calc::is_valid_coupon_date(&config, 1_000_000 + 86_400));
    assert!(calc::is_valid_coupon_date(&config, 1_000_000 - 86_400));
    assert!(!calc::is_valid_coupon_date(&config, 1_000_000 + 86_401));
    assert!(!calc::is_valid_coupon_date(&config, 1_000_000 - 86_401));
}

#[test]
fn coupon_windows_merge_when_dates_are_closer_than_the_tolerance() {
    // Not a defect, but worth pinning: the check is "near ANY scheduled date", so a
    // schedule tighter than 24h apart accepts payment across the whole merged span. The
    // fixture's dates are 4,000s apart, so a timestamp a day past the first is still
    // inside the second's window.
    let config = interest_bearing_config();
    assert!(calc::is_valid_coupon_date(&config, 5_000 + 86_401));
    // Beyond the last date's window it does close.
    assert!(!calc::is_valid_coupon_date(&config, 9_000 + 86_401));
}

// ---------------------------------------------------------------- validation

#[test]
fn a_deposit_inside_the_window_is_accepted() {
    let data = PoolData::new(discounted_config());
    assert_eq!(validate::validate_deposit(&data, 1_000, 500), Ok(()));
}

#[test]
fn a_deposit_below_the_minimum_is_rejected() {
    let data = PoolData::new(discounted_config());
    assert_eq!(
        validate::validate_deposit(&data, 999, 500),
        Err(Error::BelowMinInvestment)
    );
}

#[test]
fn a_deposit_after_funding_closes_is_rejected() {
    let data = PoolData::new(discounted_config());
    assert_eq!(
        validate::validate_deposit(&data, 1_000, EPOCH_END + 1),
        Err(Error::FundingEnded)
    );
}

#[test]
fn a_deposit_that_would_overshoot_the_target_is_rejected() {
    let mut data = PoolData::new(discounted_config());
    data.total_raised = TARGET_RAISE - 500;
    assert_eq!(
        validate::validate_deposit(&data, 1_000, 500),
        Err(Error::ExceedsTargetRaise)
    );
}

#[test]
fn deposits_are_rejected_once_the_pool_leaves_funding() {
    let data = invested_discounted();
    assert_eq!(
        validate::validate_deposit(&data, 1_000, 500),
        Err(Error::NotFundingPhase)
    );
}

#[test]
fn withdrawals_are_blocked_while_capital_is_deployed() {
    let data = invested_discounted();
    assert_eq!(
        validate::validate_withdrawal(&data, 6_000),
        Err(Error::WithdrawalsNotAllowed)
    );
}

#[test]
fn an_invested_pool_stays_shut_until_it_settles() {
    // The maturity date passing is not settlement. The router has no INVESTED branch, so
    // admitting it here only moved the rejection to a later line with a worse message.
    let data = invested_discounted();
    assert_eq!(
        validate::validate_withdrawal(&data, MATURITY),
        Err(Error::WithdrawalsNotAllowed)
    );

    // Settling is what opens redemption.
    let mut settled = invested_discounted();
    settled.status = PoolStatus::Matured;
    assert_eq!(validate::validate_withdrawal(&settled, MATURITY), Ok(()));
}

#[test]
fn withdrawals_are_open_during_funding_and_emergency() {
    let funding = PoolData::new(discounted_config());
    assert_eq!(validate::validate_withdrawal(&funding, 500), Ok(()));

    let mut emergency = invested_discounted();
    emergency.status = PoolStatus::Emergency;
    assert_eq!(validate::validate_withdrawal(&emergency, 6_000), Ok(()));
}

#[test]
fn maturity_cannot_be_processed_early_or_empty() {
    let data = invested_discounted();
    assert_eq!(
        validate::validate_maturity_processing(&data, 1_100_000, MATURITY - 1),
        Err(Error::MaturityNotReached)
    );
    assert_eq!(
        validate::validate_maturity_processing(&data, 0, MATURITY),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        validate::validate_maturity_processing(&data, 1_100_000, MATURITY),
        Ok(())
    );
}

#[test]
fn coupon_payments_require_an_invested_interest_bearing_pool() {
    let data = invested_interest_bearing();
    assert_eq!(validate::validate_coupon_payment(&data, 25_000), Ok(()));
    assert_eq!(
        validate::validate_coupon_payment(&data, 0),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        validate::validate_coupon_payment(&invested_discounted(), 25_000),
        Err(Error::NotInterestBearing)
    );
}

// ---------------------------------------------------------------- config bounds

#[test]
fn a_well_formed_config_validates() {
    assert_eq!(validate::validate_config(&discounted_config()), Ok(()));
    assert_eq!(validate::validate_config(&interest_bearing_config()), Ok(()));
}

#[test]
fn an_over_long_or_mismatched_schedule_cannot_be_constructed() {
    // The bound exists because a Solana account is sized once, at creation — and because
    // a Soroban contract has no heap. Both are enforced by the type, so a `PoolConfig`
    // carrying an invalid schedule is unrepresentable rather than merely rejected.
    let too_many: [u64; piron_core::MAX_COUPON_SCHEDULE + 1] =
        [0; piron_core::MAX_COUPON_SCHEDULE + 1];
    let rates: [u32; piron_core::MAX_COUPON_SCHEDULE + 1] =
        [100; piron_core::MAX_COUPON_SCHEDULE + 1];
    assert_eq!(
        CouponSchedule::from_parts(&too_many, &rates),
        Err(Error::CouponScheduleTooLong)
    );
    assert_eq!(
        CouponSchedule::from_parts(&[1, 2], &[250]),
        Err(Error::CouponScheduleMismatch)
    );
}

#[test]
fn maturity_must_follow_the_funding_window() {
    let mut config = discounted_config();
    config.maturity_date = config.epoch_end_time;
    assert_eq!(
        validate::validate_config(&config),
        Err(Error::InvalidMaturityDate)
    );
}
