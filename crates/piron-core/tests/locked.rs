//! Behavioural tests for Locked (fixed-term) pools.
//!
//! Mirrors `test/LockedPool.t.sol`.

use piron_core::error::Error;
use piron_core::locked::{calc, lifecycle, types::*, validate};
use piron_core::math::Amount;

const PRINCIPAL: Amount = 1_000_000;
const LOCK_START: u64 = 1_000;
/// 180 days in seconds.
const TERM: u64 = 180 * 86_400;

fn tier() -> LockTier {
    LockTier {
        duration_days: 180,
        apy_bps: 1_200, // 12% APY
        early_exit_penalty_bps: 500,
        min_deposit: 1_000,
        is_active: true,
    }
}

fn position(payment: InterestPayment) -> Position {
    calc::build_position(PRINCIPAL, &tier(), 0, payment, LOCK_START).unwrap()
}

// ---------------------------------------------------------------- interest

#[test]
fn interest_is_simple_not_compound() {
    assert_eq!(calc::interest_for_term(PRINCIPAL, 1_200, 365).unwrap(), 120_000);
    assert_eq!(calc::interest_for_term(PRINCIPAL, 1_200, 180).unwrap(), 59_178);
}

#[test]
fn a_zero_rate_or_zero_term_earns_nothing() {
    assert_eq!(calc::interest_for_term(PRINCIPAL, 0, 180).unwrap(), 0);
    assert_eq!(calc::interest_for_term(PRINCIPAL, 1_200, 0).unwrap(), 0);
}

#[test]
fn interest_accrues_linearly_through_the_term() {
    let full = 59_178;
    assert_eq!(calc::pro_rata_interest(full, 0, TERM).unwrap(), 0);
    assert_eq!(calc::pro_rata_interest(full, TERM / 2, TERM).unwrap(), 29_589);
    assert_eq!(calc::pro_rata_interest(full, TERM, TERM).unwrap(), full);
    assert_eq!(calc::pro_rata_interest(full, TERM * 2, TERM).unwrap(), full);
}

// ---------------------------------------------------------------- position shape

#[test]
fn an_upfront_position_puts_less_to_work_and_repays_principal() {
    let p = position(InterestPayment::Upfront);
    assert_eq!(p.full_interest_amount, 59_178);
    assert_eq!(p.invested_amount, 940_822);
    assert_eq!(p.expected_maturity_payout, PRINCIPAL);
    assert!(p.interest_paid);
    assert_eq!(p.interest_earned, 59_178);
}

#[test]
fn an_at_maturity_position_invests_the_lot_and_repays_more() {
    let p = position(InterestPayment::AtMaturity);
    assert_eq!(p.invested_amount, PRINCIPAL);
    assert_eq!(p.expected_maturity_payout, 1_059_178);
    assert!(!p.interest_paid);
    assert_eq!(p.interest_earned, 0);
}

#[test]
fn the_lock_runs_for_the_tier_duration() {
    let p = position(InterestPayment::AtMaturity);
    assert_eq!(p.lock_start, LOCK_START);
    assert_eq!(p.lock_end, LOCK_START + TERM);
    assert_eq!(p.apy_bps_at_deposit, 1_200);
    assert_eq!(p.status, PositionStatus::Active);
}

#[test]
fn the_rate_is_fixed_at_deposit_even_if_the_tier_changes() {
    let p = position(InterestPayment::AtMaturity);
    let mut cut = tier();
    cut.apy_bps = 100;
    assert_eq!(p.apy_bps_at_deposit, 1_200);
    assert_eq!(
        calc::build_position(PRINCIPAL, &cut, 0, InterestPayment::AtMaturity, LOCK_START)
            .unwrap()
            .apy_bps_at_deposit,
        100
    );
}

// ---------------------------------------------------------------- early exit

#[test]
fn exiting_an_upfront_position_costs_a_penalty_on_the_whole_value() {
    let p = position(InterestPayment::Upfront);
    let exit = calc::early_exit(&p, &tier(), LOCK_START + TERM / 2).unwrap();
    assert_eq!(exit.value_at_exit, 1_000_000);
    assert_eq!(exit.penalty, 50_000);
    assert_eq!(exit.payout, 890_822);
    assert_eq!(exit.interest_earned, 59_178);
}

#[test]
fn the_harshest_valid_penalty_still_returns_something() {
    // At the 50% tier ceiling: 500,000 off a 1,000,000 value, taken from the 940,822
    // still invested.
    let p = position(InterestPayment::Upfront);
    let mut punitive = tier();
    punitive.early_exit_penalty_bps = validate::MAX_TIER_BPS;

    let exit = calc::early_exit(&p, &punitive, LOCK_START + 1).unwrap();
    assert_eq!(exit.penalty, 500_000);
    assert_eq!(exit.payout, 440_822);
}

#[test]
fn the_upfront_exit_cap_is_unreachable_through_a_valid_tier() {
    // The Solidity caps the penalty at the invested balance and pays zero. Reaching that
    // needs a penalty above ~94%, which `validate_tier` rejects at 50% — so the branch is
    // dead through any tier the protocol will accept. Exercised here with an unvalidated
    // tier so the behaviour is pinned if the ceiling ever moves.
    let p = position(InterestPayment::Upfront);
    let mut absurd = tier();
    absurd.early_exit_penalty_bps = 9_500;
    assert_eq!(
        validate::validate_tier(&absurd),
        Err(Error::TierPenaltyTooHigh)
    );

    let exit = calc::early_exit(&p, &absurd, LOCK_START + 1).unwrap();
    assert_eq!(exit.payout, 0);
    assert_eq!(exit.penalty, p.invested_amount);
}

#[test]
fn exiting_an_at_maturity_position_pays_accrued_interest_less_the_penalty() {
    let p = position(InterestPayment::AtMaturity);
    let exit = calc::early_exit(&p, &tier(), LOCK_START + TERM / 2).unwrap();
    assert_eq!(exit.interest_earned, 29_589);
    assert_eq!(exit.value_at_exit, 1_029_589);
    assert_eq!(exit.penalty, 51_479);
    assert_eq!(exit.payout, 978_110);
}

#[test]
fn exiting_immediately_forfeits_the_penalty_on_principal_alone() {
    let p = position(InterestPayment::AtMaturity);
    let exit = calc::early_exit(&p, &tier(), LOCK_START).unwrap();
    assert_eq!(exit.interest_earned, 0);
    assert_eq!(exit.penalty, 50_000);
    assert_eq!(exit.payout, 950_000);
}

// ---------------------------------------------------------------- views

#[test]
fn days_remaining_counts_down_and_floors_at_zero() {
    let end = LOCK_START + TERM;
    assert_eq!(calc::days_remaining(end, LOCK_START), 180);
    assert_eq!(calc::days_remaining(end, LOCK_START + TERM / 2), 90);
    assert_eq!(calc::days_remaining(end, end), 0);
    assert_eq!(calc::days_remaining(end, end + 1_000_000), 0);
}

#[test]
fn a_summary_shows_accrued_interest_mid_term_and_the_full_amount_after() {
    let p = position(InterestPayment::AtMaturity);

    let mid = calc::position_summary(&p, LOCK_START + TERM / 2).unwrap();
    assert_eq!(mid.interest_amount, 29_589);
    assert_eq!(mid.days_remaining, 90);
    assert!(!mid.can_redeem);
    assert!(mid.can_early_exit);

    let done = calc::position_summary(&p, LOCK_START + TERM).unwrap();
    assert_eq!(done.interest_amount, 59_178);
    assert_eq!(done.days_remaining, 0);
    assert!(done.can_redeem);
    assert!(!done.can_early_exit);
}

#[test]
fn an_upfront_summary_always_shows_the_full_interest() {
    let p = position(InterestPayment::Upfront);
    let mid = calc::position_summary(&p, LOCK_START + TERM / 2).unwrap();
    assert_eq!(mid.interest_amount, 59_178);
    assert!(mid.interest_paid_upfront);
}

// ---------------------------------------------------------------- validation

#[test]
fn a_tier_must_have_a_duration_and_stay_under_the_ceilings() {
    assert_eq!(validate::validate_tier(&tier()), Ok(()));

    let mut t = tier();
    t.duration_days = 0;
    assert_eq!(validate::validate_tier(&t), Err(Error::InvalidTierDuration));

    let mut t = tier();
    t.apy_bps = validate::MAX_TIER_BPS + 1;
    assert_eq!(validate::validate_tier(&t), Err(Error::TierApyTooHigh));

    let mut t = tier();
    t.early_exit_penalty_bps = validate::MAX_TIER_BPS + 1;
    assert_eq!(validate::validate_tier(&t), Err(Error::TierPenaltyTooHigh));
}

#[test]
fn a_deposit_must_match_a_live_tier_and_clear_its_minimum() {
    assert_eq!(validate::validate_deposit(10_000, 0, &tier(), 3), Ok(()));
    assert_eq!(
        validate::validate_deposit(0, 0, &tier(), 3),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        validate::validate_deposit(10_000, 3, &tier(), 3),
        Err(Error::InvalidTier)
    );
    assert_eq!(
        validate::validate_deposit(999, 0, &tier(), 3),
        Err(Error::BelowMinInvestment)
    );

    let mut closed = tier();
    closed.is_active = false;
    assert_eq!(
        validate::validate_deposit(10_000, 0, &closed, 3),
        Err(Error::TierInactive)
    );
}

#[test]
fn an_upfront_deposit_must_be_able_to_fund_its_own_interest() {
    let interest = calc::interest_for_term(PRINCIPAL, 5_000, 1_095).unwrap();
    assert!(interest > PRINCIPAL);
    assert_eq!(
        validate::validate_upfront_capacity(PRINCIPAL, interest),
        Err(Error::InterestExceedsPrincipal)
    );
    assert_eq!(validate::validate_upfront_capacity(PRINCIPAL, 59_178), Ok(()));
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn redeeming_at_maturity_pays_principal_and_interest() {
    let p = position(InterestPayment::AtMaturity);
    let r = lifecycle::redeem(&p, LOCK_START + TERM).unwrap();
    assert_eq!(r.payout, 1_059_178);
    assert_eq!(r.interest_component, 59_178);
    assert_eq!(r.shares_burned, PRINCIPAL);
    assert_eq!(r.new_status, PositionStatus::Redeemed);
}

#[test]
fn redeeming_an_upfront_position_returns_principal_only() {
    let p = position(InterestPayment::Upfront);
    let r = lifecycle::redeem(&p, LOCK_START + TERM).unwrap();
    assert_eq!(r.payout, PRINCIPAL);
    assert_eq!(r.interest_component, 0);
}

#[test]
fn redeeming_early_is_refused() {
    let p = position(InterestPayment::AtMaturity);
    assert_eq!(
        lifecycle::redeem(&p, LOCK_START + TERM - 1),
        Err(Error::PositionNotMatured)
    );
}

#[test]
fn a_redeemed_position_cannot_be_redeemed_or_exited_again() {
    let mut p = position(InterestPayment::AtMaturity);
    p.status = PositionStatus::Redeemed;
    assert_eq!(
        lifecycle::redeem(&p, LOCK_START + TERM),
        Err(Error::PositionAlreadyRedeemed)
    );
    assert_eq!(
        lifecycle::exit_early(&p, &tier(), LOCK_START + 1),
        Err(Error::PositionAlreadyRedeemed)
    );
}

#[test]
fn exiting_early_after_maturity_is_refused() {
    let p = position(InterestPayment::AtMaturity);
    assert_eq!(
        lifecycle::exit_early(&p, &tier(), LOCK_START + TERM),
        Err(Error::PositionAlreadyMatured)
    );
}

#[test]
fn an_early_exit_records_the_penalty_it_charged() {
    let p = position(InterestPayment::AtMaturity);
    let exit = lifecycle::exit_early(&p, &tier(), LOCK_START + TERM / 2).unwrap();
    assert_eq!(exit.payout, 978_110);
    assert_eq!(exit.penalty, 51_479);
    assert_eq!(exit.shares_burned, PRINCIPAL);
    assert_eq!(exit.new_status, PositionStatus::EarlyExit);
}

// ---------------------------------------------------------------- rollover

#[test]
fn rolling_over_an_at_maturity_position_compounds() {
    let p = position(InterestPayment::AtMaturity);
    let now = LOCK_START + TERM;
    let roll = lifecycle::roll_over(&p, &tier(), 0, InterestPayment::AtMaturity, now).unwrap();

    assert_eq!(roll.principal_rolled, 1_059_178);
    assert_eq!(roll.new_position.principal_deposited, 1_059_178);
    assert_eq!(roll.new_position.full_interest_amount, 62_680);
    assert_eq!(roll.new_position.expected_maturity_payout, 1_121_858);
    assert_eq!(roll.closed_status, PositionStatus::RolledOver);
    assert_eq!(roll.new_position.lock_start, now);
}

#[test]
fn rolling_over_an_upfront_position_does_not_compound() {
    let p = position(InterestPayment::Upfront);
    let now = LOCK_START + TERM;
    let roll = lifecycle::roll_over(&p, &tier(), 0, InterestPayment::Upfront, now).unwrap();
    assert_eq!(roll.principal_rolled, PRINCIPAL);
    assert_eq!(roll.new_position.full_interest_amount, 59_178);
}

#[test]
fn a_rollover_into_a_closed_tier_is_refused() {
    let p = position(InterestPayment::AtMaturity);
    let mut closed = tier();
    closed.is_active = false;
    assert_eq!(
        lifecycle::roll_over(&p, &closed, 0, InterestPayment::AtMaturity, LOCK_START + TERM),
        Err(Error::TierInactive)
    );
}

#[test]
fn an_immature_position_cannot_be_rolled() {
    let p = position(InterestPayment::AtMaturity);
    assert_eq!(
        lifecycle::roll_over(&p, &tier(), 0, InterestPayment::AtMaturity, LOCK_START + 1),
        Err(Error::PositionNotMatured)
    );
}
