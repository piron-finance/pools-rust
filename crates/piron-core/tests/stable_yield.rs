//! Behavioural tests for Stable Yield pools.
//!
//! Mirrors `test/StableYieldPool.t.sol`.

use piron_core::error::Error;
use piron_core::math::Amount;
use piron_core::stable_yield::{nav, queue, types::*, validate};

const PURCHASE: u64 = 1_000;
const YEAR: u64 = 365 * 86_400;

fn discounted() -> InstrumentHolding {
    InstrumentHolding {
        instrument_type: InstrumentType::Discounted,
        purchase_price: 950_000,
        face_value: 1_000_000,
        purchase_date: PURCHASE,
        maturity_date: PURCHASE + YEAR,
        annual_coupon_rate: 0,
        coupon_frequency: 0,
        next_coupon_due_date: 0,
        coupons_paid: 0,
        is_active: true,
    }
}

fn interest_bearing() -> InstrumentHolding {
    InstrumentHolding {
        instrument_type: InstrumentType::InterestBearing,
        purchase_price: 1_000_000,
        face_value: 1_000_000,
        purchase_date: PURCHASE,
        maturity_date: PURCHASE + YEAR,
        annual_coupon_rate: 800, // 8%
        coupon_frequency: 4,     // quarterly
        next_coupon_due_date: PURCHASE + YEAR / 4,
        coupons_paid: 0,
        is_active: true,
    }
}

fn reserve_config() -> ReserveConfig {
    ReserveConfig {
        min_absolute_reserve: 50_000,
        reserve_ratio_bps: 1_000, // 10%
    }
}

// ---------------------------------------------------------------- instruments

#[test]
fn a_discounted_instrument_accretes_from_price_to_face() {
    let i = discounted();
    assert_eq!(nav::instrument_value(&i, PURCHASE).unwrap(), 950_000);
    assert_eq!(nav::instrument_value(&i, PURCHASE + YEAR / 2).unwrap(), 975_000);
    assert_eq!(nav::instrument_value(&i, PURCHASE + YEAR).unwrap(), 1_000_000);
}

#[test]
fn a_matured_discounted_instrument_sits_at_face() {
    let i = discounted();
    assert_eq!(
        nav::instrument_value(&i, PURCHASE + YEAR * 3).unwrap(),
        1_000_000
    );
}

#[test]
fn an_interest_bearing_instrument_accrues_its_coupon() {
    let i = interest_bearing();
    // 8% of 1,000,000 over four coupons is 20,000 a quarter.
    assert_eq!(nav::instrument_value(&i, PURCHASE).unwrap(), 1_000_000);
    assert_eq!(
        nav::instrument_value(&i, PURCHASE + YEAR / 8).unwrap(),
        1_010_000
    );
    assert_eq!(
        nav::instrument_value(&i, PURCHASE + YEAR / 4).unwrap(),
        1_020_000
    );
}

#[test]
fn accrual_restarts_from_the_last_coupon_paid() {
    let mut i = interest_bearing();
    i.coupons_paid = 1;
    i.next_coupon_due_date = PURCHASE + YEAR / 2;
    // Half a period past the first coupon.
    assert_eq!(
        nav::instrument_value(&i, PURCHASE + YEAR / 4 + YEAR / 8).unwrap(),
        1_010_000
    );
}

#[test]
fn an_interest_bearing_instrument_needs_a_coupon_frequency() {
    let mut i = interest_bearing();
    i.coupon_frequency = 0;
    assert_eq!(
        nav::instrument_value(&i, PURCHASE),
        Err(Error::InvalidCouponFrequency)
    );
}

// ---------------------------------------------------------------- portfolio

#[test]
fn gross_asset_value_sums_the_active_holdings() {
    let holdings = [discounted(), interest_bearing()];
    let value = nav::gross_asset_value(&holdings, PURCHASE + YEAR / 8).unwrap();
    // An eighth of the way: 950,000 + 6,250 accreted, plus 1,000,000 + 10,000 accrued.
    assert_eq!(value, 956_250 + 1_010_000);
}

#[test]
fn a_closed_holding_contributes_nothing() {
    let mut closed = discounted();
    closed.is_active = false;
    let holdings = [closed, interest_bearing()];
    assert_eq!(
        nav::gross_asset_value(&holdings, PURCHASE).unwrap(),
        1_000_000
    );
}

#[test]
fn an_empty_portfolio_is_worth_nothing() {
    assert_eq!(nav::gross_asset_value(&[], PURCHASE).unwrap(), 0);
}

#[test]
fn pool_nav_adds_uninvested_cash_to_the_portfolio() {
    assert_eq!(nav::pool_nav(1_900_000, 100_000).unwrap(), 2_000_000);
}

// ---------------------------------------------------------------- pricing

#[test]
fn an_empty_pool_prices_at_one() {
    assert_eq!(nav::nav_per_share(0, 0).unwrap(), nav::NAV_PRECISION);
    // Even with assets, no shares means the next depositor sets the price.
    assert_eq!(nav::nav_per_share(500_000, 0).unwrap(), nav::NAV_PRECISION);
}

#[test]
fn nav_per_share_tracks_the_portfolio() {
    assert_eq!(
        nav::nav_per_share(1_000_000, 1_000_000).unwrap(),
        nav::NAV_PRECISION
    );
    // A 10% gain lifts the share price 10%.
    assert_eq!(
        nav::nav_per_share(1_100_000, 1_000_000).unwrap(),
        nav::NAV_PRECISION * 11 / 10
    );
}

#[test]
fn a_deposit_buys_fewer_shares_once_the_pool_has_gained() {
    let price = nav::nav_per_share(1_100_000, 1_000_000).unwrap();
    // 100,000 at 1.1 buys 90,909 shares, not 100,000.
    assert_eq!(nav::shares_for_deposit(100_000, price).unwrap(), 90_909);
}

#[test]
fn shares_and_assets_round_trip_within_a_unit() {
    let price = nav::nav_per_share(1_100_000, 1_000_000).unwrap();
    let shares = nav::shares_for_deposit(100_000, price).unwrap();
    let back = nav::assets_for_shares(shares, price).unwrap();
    // Truncation loses at most one unit, and always in the pool's favour.
    assert!(back <= 100_000 && back >= 99_999, "got {back}");
}

#[test]
fn a_zero_price_is_refused_rather_than_dividing_by_zero() {
    assert_eq!(
        nav::shares_for_deposit(100_000, 0),
        Err(Error::InvalidNavPerShare)
    );
    assert_eq!(
        nav::assets_for_shares(100_000, 0),
        Err(Error::InvalidNavPerShare)
    );
}

// ---------------------------------------------------------------- reserves

#[test]
fn the_reserve_is_the_greater_of_the_floor_and_the_ratio() {
    // 10% of 1,000,000 beats the 50,000 floor.
    assert_eq!(
        validate::required_reserve(1_000_000, &reserve_config()).unwrap(),
        100_000
    );
    // On a small pool the floor binds instead.
    assert_eq!(
        validate::required_reserve(100_000, &reserve_config()).unwrap(),
        50_000
    );
}

#[test]
fn only_cash_above_the_reserve_can_be_deployed() {
    assert_eq!(
        validate::deployable_amount(300_000, 1_000_000, &reserve_config()).unwrap(),
        200_000
    );
    // Already at or under the floor: nothing is free.
    assert_eq!(
        validate::deployable_amount(100_000, 1_000_000, &reserve_config()).unwrap(),
        0
    );
    assert_eq!(
        validate::deployable_amount(50_000, 1_000_000, &reserve_config()).unwrap(),
        0
    );
}

#[test]
fn a_deployment_that_would_breach_the_reserve_is_refused() {
    let cfg = reserve_config();
    assert_eq!(validate::validate_deployment(200_000, 300_000, 1_000_000, &cfg), Ok(()));
    assert_eq!(
        validate::validate_deployment(200_001, 300_000, 1_000_000, &cfg),
        Err(Error::ReserveBreached)
    );
}

#[test]
fn a_holder_must_sit_out_the_holding_period() {
    let period = 7 * 86_400;
    assert_eq!(
        validate::validate_holding_period(1_000, period, 1_000 + period - 1),
        Err(Error::HoldingPeriodNotMet)
    );
    assert_eq!(
        validate::validate_holding_period(1_000, period, 1_000 + period),
        Ok(())
    );
}

#[test]
fn deposits_are_refused_when_the_pool_is_closed_or_the_ticket_is_small() {
    assert_eq!(validate::validate_deposit(10_000, 1_000, true), Ok(()));
    assert_eq!(
        validate::validate_deposit(10_000, 1_000, false),
        Err(Error::PoolInactive)
    );
    assert_eq!(
        validate::validate_deposit(999, 1_000, true),
        Err(Error::BelowMinInvestment)
    );
}

// ---------------------------------------------------------------- the queue

fn request(shares: Amount) -> WithdrawalRequest {
    WithdrawalRequest {
        shares,
        request_time: 1_000,
        deadline: 0,
        estimated_value: shares,
        fee_amount: 0,
        processed: false,
        processed_time: 0,
    }
}

fn queue_of(n: u64) -> WithdrawalQueue {
    WithdrawalQueue {
        head: 0,
        tail: n,
        total_pending_value: 0,
    }
}

#[test]
fn a_withdrawal_is_quoted_net_of_its_fee() {
    let quote = queue::quote_withdrawal(100_000, nav::NAV_PRECISION, 50).unwrap();
    assert_eq!(quote.gross_value, 100_000);
    assert_eq!(quote.fee, 500);
    assert_eq!(quote.net_value, 99_500);
}

#[test]
fn enqueueing_advances_the_tail_and_the_pending_total() {
    let q = queue_of(0);
    let quote = queue::quote_withdrawal(100_000, nav::NAV_PRECISION, 50).unwrap();
    let q = queue::enqueue(&q, &quote).unwrap();
    assert_eq!(q.tail, 1);
    assert_eq!(q.len(), 1);
    assert_eq!(q.total_pending_value, 99_500);
}

#[test]
fn a_batch_serves_in_order_and_stops_when_liquidity_runs_out() {
    let q = queue_of(3);
    let requests = [request(100_000), request(100_000), request(100_000)];

    let out = queue::process_batch(&q, &requests, 250_000, nav::NAV_PRECISION, 50, 20).unwrap();

    // Two fit; the third needs 100,000 against 50,000 left.
    assert_eq!(out.processed, 2);
    assert_eq!(out.total_paid, 199_000);
    assert_eq!(out.total_fees, 1_000);
    assert_eq!(out.shares_burned, 200_000);
    assert_eq!(out.new_head, 2);
    assert!(out.stopped_for_liquidity);
}

#[test]
fn a_large_request_blocks_the_queue_rather_than_being_skipped() {
    let q = queue_of(2);
    // The first is unaffordable; the second would fit but must wait its turn.
    let requests = [request(500_000), request(10_000)];

    let out = queue::process_batch(&q, &requests, 100_000, nav::NAV_PRECISION, 0, 20).unwrap();
    assert_eq!(out.processed, 0);
    assert_eq!(out.new_head, 0);
    assert!(out.stopped_for_liquidity);
}

#[test]
fn a_batch_stops_at_the_size_cap_without_flagging_liquidity() {
    let q = queue_of(3);
    let requests = [request(10_000), request(10_000), request(10_000)];

    let out = queue::process_batch(&q, &requests, 1_000_000, nav::NAV_PRECISION, 0, 2).unwrap();
    assert_eq!(out.processed, 2);
    assert_eq!(out.new_head, 2);
    assert!(!out.stopped_for_liquidity);
}

#[test]
fn a_batch_larger_than_the_footprint_allows_is_refused() {
    let q = queue_of(1);
    assert_eq!(
        queue::process_batch(
            &q,
            &[request(1)],
            1_000_000,
            nav::NAV_PRECISION,
            0,
            queue::MAX_WITHDRAWAL_BATCH + 1
        ),
        Err(Error::BatchTooLarge)
    );
}

#[test]
fn an_empty_queue_has_nothing_to_process() {
    assert_eq!(
        queue::process_batch(&queue_of(0), &[], 1_000_000, nav::NAV_PRECISION, 0, 20),
        Err(Error::QueueEmpty)
    );
}

#[test]
fn an_already_served_request_is_stepped_over_not_paid_twice() {
    let q = queue_of(2);
    let mut done = request(100_000);
    done.processed = true;
    let requests = [done, request(100_000)];

    let out = queue::process_batch(&q, &requests, 1_000_000, nav::NAV_PRECISION, 0, 20).unwrap();
    assert_eq!(out.processed, 1);
    assert_eq!(out.total_paid, 100_000);
    assert_eq!(out.new_head, 2);
}

#[test]
fn withdrawals_are_repriced_when_served_not_when_queued() {
    // Queued at par, served after a 10% gain: the holder gets the gain, which is what
    // stops queueing being a free option on the NAV.
    let q = queue_of(1);
    let requests = [request(100_000)];
    let higher = nav::nav_per_share(1_100_000, 1_000_000).unwrap();

    let out = queue::process_batch(&q, &requests, 1_000_000, higher, 0, 20).unwrap();
    assert_eq!(out.total_paid, 110_000);
}

#[test]
fn a_request_past_its_deadline_is_expired() {
    let mut r = request(100_000);
    r.deadline = 5_000;
    assert!(!queue::is_expired(&r, 5_000));
    assert!(queue::is_expired(&r, 5_001));
    // A deadline of zero means it never expires.
    r.deadline = 0;
    assert!(!queue::is_expired(&r, u64::MAX));
}
