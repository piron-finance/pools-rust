//! State machine and cash-flow tests for Single-Asset pools.
//!
//! Mirrors `test/SingleAssetPool.t.sol` and `test/Integration.t.sol`.

use piron_core::error::Error;
use piron_core::math::Amount;
use piron_core::single_asset::{calc, flow, lifecycle};
use piron_core::schedule::CouponSchedule;
use piron_core::types::{InstrumentType, PoolConfig, PoolData, PoolStatus};

const EPOCH_END: u64 = 1_000;
const MATURITY: u64 = 11_000;
const TARGET: Amount = 1_000_000;

fn config(instrument: InstrumentType) -> PoolConfig {
    PoolConfig {
        instrument_type: instrument,
        face_value: 0,
        purchase_price: 1_000_000,
        target_raise: TARGET,
        epoch_end_time: EPOCH_END,
        maturity_date: MATURITY,
        coupons: if instrument == InstrumentType::Discounted {
            CouponSchedule::empty()
        } else {
            CouponSchedule::from_parts(&[5_000, 9_000], &[250, 250]).unwrap()
        },
        discount_rate: 500,
        minimum_funding_threshold: 8_000, // 80%
        min_investment: 1_000,
        withdrawal_fee_bps: 0,
    }
}

fn funding(instrument: InstrumentType, raised: Amount) -> PoolData {
    let mut data = PoolData::new(config(instrument));
    data.total_raised = raised;
    data
}

fn pending(instrument: InstrumentType, raised: Amount) -> PoolData {
    let mut data = funding(instrument, raised);
    data.status = PoolStatus::PendingInvestment;
    data
}

// ---------------------------------------------------------------- filling

#[test]
fn a_pool_fills_when_it_reaches_target() {
    let data = funding(InstrumentType::Discounted, TARGET);
    assert_eq!(lifecycle::mark_filled(&data), Ok(PoolStatus::Filled));
}

#[test]
fn a_pool_below_target_does_not_fill() {
    let data = funding(InstrumentType::Discounted, TARGET - 1);
    assert_eq!(lifecycle::mark_filled(&data), Err(Error::TargetNotReached));
}

// ---------------------------------------------------------------- epoch close

#[test]
fn the_minimum_raise_is_a_share_of_target() {
    // 80% of 1,000,000.
    assert_eq!(
        lifecycle::minimum_funding_required(&config(InstrumentType::Discounted)).unwrap(),
        800_000
    );
}

#[test]
fn funding_cannot_close_before_the_epoch_ends() {
    let data = funding(InstrumentType::Discounted, TARGET);
    assert_eq!(
        lifecycle::close_epoch(&data, EPOCH_END - 1, false),
        Err(Error::EpochNotEnded)
    );
}

#[test]
fn a_filled_pool_closes_without_waiting_for_the_epoch() {
    let mut data = funding(InstrumentType::Discounted, TARGET);
    data.status = PoolStatus::Filled;
    let close = lifecycle::close_epoch(&data, 1, false).unwrap();
    assert_eq!(close.new_status, PoolStatus::PendingInvestment);
}

#[test]
fn closing_a_funded_discounted_epoch_fixes_face_value() {
    let data = funding(InstrumentType::Discounted, 900_000);
    let close = lifecycle::close_epoch(&data, EPOCH_END, false).unwrap();
    assert_eq!(close.new_status, PoolStatus::PendingInvestment);
    assert!(!close.underfunded);
    // 900,000 at a 5% discount.
    assert_eq!(close.face_value, 947_368);
}

#[test]
fn closing_an_interest_bearing_epoch_leaves_face_value_at_zero() {
    let data = funding(InstrumentType::InterestBearing, 900_000);
    let close = lifecycle::close_epoch(&data, EPOCH_END, false).unwrap();
    assert_eq!(close.face_value, 0);
    assert_eq!(close.new_status, PoolStatus::PendingInvestment);
}

#[test]
fn an_underfunded_epoch_goes_to_emergency() {
    let data = funding(InstrumentType::Discounted, 799_999);
    let close = lifecycle::close_epoch(&data, EPOCH_END, false).unwrap();
    assert_eq!(close.new_status, PoolStatus::Emergency);
    assert!(close.underfunded);
    assert_eq!(close.face_value, 0);
}

#[test]
fn a_force_close_skips_the_epoch_check() {
    let data = funding(InstrumentType::Discounted, TARGET);
    let close = lifecycle::close_epoch(&data, 1, true).unwrap();
    assert_eq!(close.new_status, PoolStatus::PendingInvestment);
}

#[test]
fn a_force_close_only_applies_to_a_funding_pool() {
    let mut data = funding(InstrumentType::Discounted, TARGET);
    data.status = PoolStatus::Filled;
    assert_eq!(
        lifecycle::close_epoch(&data, 1, true),
        Err(Error::NotFundingPhase)
    );
}

// ---------------------------------------------------------------- investment

#[test]
fn spv_withdrawal_requires_a_pending_pool() {
    assert_eq!(
        lifecycle::validate_spv_withdrawal(&pending(InstrumentType::Discounted, TARGET), 500_000),
        Ok(())
    );
    assert_eq!(
        lifecycle::validate_spv_withdrawal(&funding(InstrumentType::Discounted, TARGET), 500_000),
        Err(Error::NotPendingInvestment)
    );
}

#[test]
fn confirming_a_discounted_investment_fixes_face_value_and_discount() {
    let data = pending(InstrumentType::Discounted, TARGET);
    let outcome = lifecycle::process_investment(&data, 1_000_000, 500).unwrap();
    assert_eq!(outcome.new_status, PoolStatus::Invested);
    assert_eq!(outcome.actual_invested, 1_000_000);
    assert_eq!(outcome.face_value, 1_052_631);
    assert_eq!(outcome.total_discount_earned, 52_631);
}

#[test]
fn the_spv_cannot_invest_more_than_was_raised() {
    let data = pending(InstrumentType::Discounted, TARGET);
    assert_eq!(
        lifecycle::process_investment(&data, TARGET + 1, 500),
        Err(Error::InvestmentExceedsRaised)
    );
}

#[test]
fn an_interest_bearing_investment_rejects_a_coupon_date_already_past() {
    let data = pending(InstrumentType::InterestBearing, TARGET);
    // First coupon is at 5,000; confirming at 6,000 would leave it unpayable.
    assert_eq!(
        lifecycle::process_investment(&data, TARGET, 6_000),
        Err(Error::CouponDateInPast)
    );
}

#[test]
fn an_interest_bearing_investment_with_a_future_schedule_is_accepted() {
    let data = pending(InstrumentType::InterestBearing, TARGET);
    let outcome = lifecycle::process_investment(&data, TARGET, 500).unwrap();
    assert_eq!(outcome.face_value, 0);
    assert_eq!(outcome.total_discount_earned, 0);
    assert_eq!(outcome.actual_invested, TARGET);
}

// ---------------------------------------------------------------- maturity

fn invested_discounted() -> PoolData {
    let mut data = pending(InstrumentType::Discounted, TARGET);
    data.status = PoolStatus::Invested;
    data.actual_invested = 1_000_000;
    data.config.face_value = 1_100_000;
    data.total_discount_earned = 100_000;
    data
}

#[test]
fn a_return_matching_expectation_is_on_target() {
    let data = invested_discounted();
    let outcome = lifecycle::process_maturity(&data, 1_100_000, MATURITY, 1_000_000).unwrap();
    assert_eq!(outcome.new_status, PoolStatus::Matured);
    assert_eq!(outcome.funds_returned, 1_100_000);
    assert_eq!(outcome.variance, lifecycle::MaturityVariance::OnTarget);
}

#[test]
fn a_short_return_reports_the_gap() {
    let data = invested_discounted();
    let outcome = lifecycle::process_maturity(&data, 1_000_000, MATURITY, 1_000_000).unwrap();
    assert_eq!(
        outcome.variance,
        lifecycle::MaturityVariance::Shortfall(100_000)
    );
}

#[test]
fn a_return_over_a_hundred_and_twenty_percent_is_flagged() {
    let data = invested_discounted();
    // 120% of 1,100,000 is 1,320,000 — the boundary itself is not an overage.
    assert_eq!(
        lifecycle::process_maturity(&data, 1_320_000, MATURITY, 1_000_000)
            .unwrap()
            .variance,
        lifecycle::MaturityVariance::OnTarget
    );
    assert_eq!(
        lifecycle::process_maturity(&data, 1_320_001, MATURITY, 1_000_000)
            .unwrap()
            .variance,
        lifecycle::MaturityVariance::Overage
    );
}

#[test]
fn maturity_cannot_be_settled_early() {
    let data = invested_discounted();
    assert_eq!(
        lifecycle::process_maturity(&data, 1_100_000, MATURITY - 1, 1_000_000),
        Err(Error::MaturityNotReached)
    );
}

#[test]
fn a_pool_closes_only_once_every_share_is_redeemed() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Matured;
    assert_eq!(
        lifecycle::mark_withdrawn(&data, 1),
        Err(Error::SharesStillOutstanding)
    );
    assert_eq!(lifecycle::mark_withdrawn(&data, 0), Ok(PoolStatus::Withdrawn));
}

#[test]
fn emergency_is_reachable_from_any_live_state_but_not_a_finished_one() {
    assert_eq!(
        lifecycle::declare_emergency(&funding(InstrumentType::Discounted, 0)),
        Ok(PoolStatus::Emergency)
    );
    assert_eq!(
        lifecycle::declare_emergency(&invested_discounted()),
        Ok(PoolStatus::Emergency)
    );

    let mut done = invested_discounted();
    done.status = PoolStatus::Withdrawn;
    assert_eq!(
        lifecycle::declare_emergency(&done),
        Err(Error::InvalidStatusForOperation)
    );
}

// ---------------------------------------------------------------- deposits

#[test]
fn a_deposit_mints_one_for_one_and_raises_the_total() {
    let data = funding(InstrumentType::Discounted, 100_000);
    let outcome = flow::deposit(&data, 50_000, 500, 0).unwrap();
    assert_eq!(outcome.shares, 50_000);
    assert_eq!(outcome.total_raised, 150_000);
    assert!(outcome.starts_position);
}

#[test]
fn a_repeat_deposit_does_not_restamp_the_position() {
    let data = funding(InstrumentType::Discounted, 100_000);
    let outcome = flow::deposit(&data, 50_000, 500, 400).unwrap();
    assert!(!outcome.starts_position);
}

// ---------------------------------------------------------------- withdrawals

#[test]
fn cancelling_during_funding_refunds_at_par() {
    let data = funding(InstrumentType::Discounted, 500_000);
    let outcome = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 40_000,
            user_shares: 100_000,
            total_shares: 500_000,
            now: 500,
        },
    )
    .unwrap();

    assert_eq!(outcome.kind, flow::WithdrawalKind::Funding);
    assert_eq!(outcome.shares_burned, 40_000);
    assert_eq!(outcome.net, 40_000);
    assert_eq!(outcome.fee, 0);
    assert_eq!(outcome.total_raised, 460_000);
    assert!(!outcome.clears_position);
}

#[test]
fn cancelling_the_whole_position_clears_it() {
    let data = funding(InstrumentType::Discounted, 500_000);
    let outcome = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 100_000,
            user_shares: 100_000,
            total_shares: 500_000,
            now: 500,
        },
    )
    .unwrap();
    assert!(outcome.clears_position);
}

#[test]
fn cancelling_after_the_epoch_ends_is_rejected() {
    let data = funding(InstrumentType::Discounted, 500_000);
    assert_eq!(
        flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: 40_000,
                user_shares: 100_000,
                total_shares: 500_000,
                now: EPOCH_END + 1,
            },
        ),
        Err(Error::FundingEnded)
    );
}

#[test]
fn cancelling_more_than_you_hold_is_rejected() {
    let data = funding(InstrumentType::Discounted, 500_000);
    assert_eq!(
        flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: 200_000,
                user_shares: 100_000,
                total_shares: 500_000,
                now: 500,
            },
        ),
        Err(Error::InsufficientShares)
    );
}

#[test]
fn an_invested_pool_permits_no_withdrawal_even_past_maturity() {
    // PARITY: validateWithdrawal would allow this; handleWithdraw rejects it. The
    // rejection governs — a pool must be settled to MATURED first.
    let data = invested_discounted();
    assert_eq!(
        flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: 1,
                user_shares: 100_000,
                total_shares: 1_000_000,
                now: MATURITY + 1,
            },
        ),
        Err(Error::WithdrawalsNotAllowed)
    );
}

#[test]
fn redeeming_a_matured_pool_pays_pro_rata_less_the_fee() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Matured;
    data.config.withdrawal_fee_bps = 50; // 0.5%
    // The SPV settled at face; the pot is what it returned, and the divisor was frozen then.
    data.funds_returned_by_spv = 1_100_000;
    data.shares_at_maturity = 1_000_000;

    let outcome = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 1, // ignored on this path
            user_shares: 100_000,
            total_shares: 1_000_000,
            now: MATURITY,
        },
    )
    .unwrap();

    assert_eq!(outcome.kind, flow::WithdrawalKind::Matured);
    // 10% of 1,100,000 redeemable.
    assert_eq!(outcome.gross, 110_000);
    assert_eq!(outcome.fee, 550);
    assert_eq!(outcome.net, 109_450);
    // The whole position goes, whatever was asked for.
    assert_eq!(outcome.shares_burned, 100_000);
    assert!(outcome.clears_position);
}

#[test]
fn redeeming_before_the_maturity_date_is_rejected() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Matured;
    assert_eq!(
        flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: 1,
                user_shares: 100_000,
                total_shares: 1_000_000,
                now: MATURITY - 1,
            },
        ),
        Err(Error::MaturityNotReached)
    );
}

#[test]
fn an_emergency_refund_returns_par_and_allows_a_partial() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Emergency;

    let outcome = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 40_000,
            user_shares: 100_000,
            total_shares: 1_000_000,
            now: 6_000,
        },
    )
    .unwrap();

    assert_eq!(outcome.kind, flow::WithdrawalKind::Emergency);
    assert_eq!(outcome.net, 40_000);
    assert_eq!(outcome.fee, 0);
    assert!(!outcome.clears_position);
}

#[test]
fn an_emergency_refund_cannot_exceed_the_position() {
    let mut data = invested_discounted();
    data.status = PoolStatus::Emergency;
    assert_eq!(
        flow::withdraw(
            &data,
            flow::WithdrawRequest {
                assets: 100_001,
                user_shares: 100_000,
                total_shares: 1_000_000,
                now: 6_000,
            },
        ),
        Err(Error::ExceedsRefundAmount)
    );
}

// ---------------------------------------------------------------- settlement

#[test]
fn settlement_pays_what_the_spv_returned_not_the_projection() {
    // One formula, both instrument kinds: what actually came back, plus coupons received
    // and not yet distributed. Face value and scheduled coupons are targets, and paying
    // them out when the instrument underperformed overpays whoever exits first.
    let mut data = invested_discounted();
    data.status = PoolStatus::Matured;
    data.funds_returned_by_spv = 1_050_000;
    assert_eq!(calc::total_returns(&data).unwrap(), 1_050_000);

    let mut ib = pending(InstrumentType::InterestBearing, TARGET);
    ib.status = PoolStatus::Matured;
    ib.actual_invested = 1_000_000;
    ib.total_coupons_received = 25_000;
    ib.total_coupons_distributed = 25_000;
    ib.funds_returned_by_spv = 1_000_000;
    assert_eq!(calc::total_returns(&ib).unwrap(), 1_000_000);
}

#[test]
fn a_shortfall_is_shared_rather_than_taken_by_whoever_exits_first() {
    // The SPV returns 900k against a 1.1m face value. Two holders at 60/40 must take
    // 540k and 360k — not 660k and then whatever is left.
    let mut data = invested_discounted();
    data.status = PoolStatus::Matured;
    data.funds_returned_by_spv = 900_000;
    data.shares_at_maturity = 1_000_000;
    data.config.withdrawal_fee_bps = 0;

    let first = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 600_000,
            user_shares: 600_000,
            total_shares: 1_000_000,
            now: MATURITY,
        },
    )
    .unwrap();
    assert_eq!(first.gross, 540_000);

    // Supply has shrunk to 400_000, but the divisor is frozen at settlement.
    let second = flow::withdraw(
        &data,
        flow::WithdrawRequest {
            assets: 400_000,
            user_shares: 400_000,
            total_shares: 400_000,
            now: MATURITY,
        },
    )
    .unwrap();
    assert_eq!(second.gross, 360_000);
    assert_eq!(first.gross + second.gross, 900_000, "the pot is exactly consumed");
}

#[test]
fn emergency_is_refused_once_the_escrow_has_been_drawn_down() {
    // While nothing has left, a par refund is honourable: the escrow holds every unit
    // raised, and one unit per share is exactly what is owed.
    let mut whole = invested_discounted();
    whole.status = PoolStatus::Invested;
    whole.funds_withdrawn_by_spv = 0;
    assert_eq!(
        lifecycle::declare_emergency(&whole),
        Ok(PoolStatus::Emergency)
    );

    // Once the SPV holds the money, par is a promise the escrow cannot keep. Whoever
    // claims first would take their full face and leave the rest with an empty escrow.
    let mut drawn = invested_discounted();
    drawn.status = PoolStatus::Invested;
    drawn.funds_withdrawn_by_spv = 1;
    assert_eq!(
        lifecycle::declare_emergency(&drawn),
        Err(Error::InvalidStatusForOperation)
    );
}
