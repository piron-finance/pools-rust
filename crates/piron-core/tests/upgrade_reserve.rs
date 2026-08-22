//! Tests for upgrade governance and the protocol reserve.

use piron_core::error::Error;
use piron_core::governance::{reserve, upgrade};
use piron_core::math::Amount;

const NOW: u64 = 1_000_000;

// ---------------------------------------------------------------- upgrade timelock

#[test]
fn the_three_delays_are_what_the_solidity_says() {
    assert_eq!(upgrade::UPGRADE_DELAY, 72 * 60 * 60);
    assert_eq!(upgrade::GRACE_PERIOD, 7 * 24 * 60 * 60);
    assert_eq!(upgrade::MAX_PAUSE_DURATION, 30 * 24 * 60 * 60);
}

#[test]
fn an_upgrade_waits_then_opens_then_expires() {
    let op = upgrade::UpgradeOperation::new(NOW);

    assert_eq!(op.state(NOW), upgrade::OperationState::Waiting);
    assert_eq!(op.state(op.ready_at() - 1), upgrade::OperationState::Waiting);
    assert_eq!(op.state(op.ready_at()), upgrade::OperationState::Ready);
    assert_eq!(op.state(op.expires_at()), upgrade::OperationState::Ready);
    assert_eq!(
        op.state(op.expires_at() + 1),
        upgrade::OperationState::Expired
    );
}

#[test]
fn execution_is_refused_outside_the_window() {
    let op = upgrade::UpgradeOperation::new(NOW);
    assert_eq!(
        upgrade::validate_execution(&op, None, NOW),
        Err(Error::UpgradeDelayNotElapsed)
    );
    assert_eq!(upgrade::validate_execution(&op, None, op.ready_at()), Ok(()));
    assert_eq!(
        upgrade::validate_execution(&op, None, op.expires_at() + 1),
        Err(Error::UpgradeExpired)
    );
}

#[test]
fn a_stale_approval_is_not_an_approval() {
    // Code reviewed three months ago has to go round again.
    let op = upgrade::UpgradeOperation::new(NOW);
    assert_eq!(
        upgrade::validate_execution(&op, None, NOW + 90 * 24 * 60 * 60),
        Err(Error::UpgradeExpired)
    );
}

#[test]
fn an_executed_or_cancelled_upgrade_cannot_run_again() {
    let mut op = upgrade::UpgradeOperation::new(NOW);
    let ready = op.ready_at();

    op.executed = true;
    assert_eq!(
        upgrade::validate_execution(&op, None, ready),
        Err(Error::UpgradeAlreadyExecuted)
    );
    assert_eq!(
        upgrade::validate_cancellation(&op),
        Err(Error::UpgradeAlreadyExecuted)
    );

    op.executed = false;
    op.cancelled = true;
    assert_eq!(
        upgrade::validate_execution(&op, None, ready),
        Err(Error::UpgradeCancelled)
    );
}

#[test]
fn a_guardian_pause_blocks_execution() {
    let op = upgrade::UpgradeOperation::new(NOW);
    let ready = op.ready_at();
    assert_eq!(
        upgrade::validate_execution(&op, Some(ready), ready),
        Err(Error::UpgradesPaused)
    );
}

#[test]
fn a_pause_lapses_on_its_own_after_thirty_days() {
    // A guardian can stop an upgrade; a guardian cannot freeze governance forever.
    let paused_at = NOW;
    assert!(upgrade::pause_is_active(Some(paused_at), NOW));
    assert!(upgrade::pause_is_active(
        Some(paused_at),
        paused_at + upgrade::MAX_PAUSE_DURATION - 1
    ));
    assert!(!upgrade::pause_is_active(
        Some(paused_at),
        paused_at + upgrade::MAX_PAUSE_DURATION
    ));
    assert!(!upgrade::pause_is_active(None, NOW));
}

#[test]
fn an_upgrade_scheduled_before_a_pause_runs_once_the_pause_lapses() {
    let paused_at = NOW;
    let op = upgrade::UpgradeOperation::new(paused_at + upgrade::MAX_PAUSE_DURATION);
    let ready = op.ready_at();
    assert_eq!(upgrade::validate_execution(&op, Some(paused_at), ready), Ok(()));
}

// ---------------------------------------------------------------- reserve

fn config(floor: Amount, treasury_bps: u32) -> reserve::ReserveConfig {
    reserve::ReserveConfig {
        min_floor: floor,
        treasury_split_bps: treasury_bps,
    }
}

fn state(balance: Amount, loaned: Amount) -> reserve::ReserveState {
    reserve::ReserveState {
        balance,
        total_loaned: loaned,
        ..Default::default()
    }
}

#[test]
fn incoming_yield_splits_between_treasury_and_reserve() {
    let split = reserve::split_yield(100_000, &config(0, 3_000)).unwrap();
    assert_eq!(split.to_treasury, 30_000);
    assert_eq!(split.retained, 70_000);
}

#[test]
fn a_yield_split_never_loses_a_unit() {
    for amount in [1, 7, 999, 1_000_001] {
        let split = reserve::split_yield(amount, &config(0, 3_333)).unwrap();
        assert_eq!(split.to_treasury + split.retained, amount, "amount {amount}");
    }
}

#[test]
fn the_reserve_lends_only_what_sits_above_its_floor() {
    let cfg = config(100_000, 0);
    assert_eq!(reserve::available_for_loan(&state(500_000, 0), &cfg), 400_000);
    assert_eq!(reserve::available_for_loan(&state(100_000, 0), &cfg), 0);
    assert_eq!(reserve::available_for_loan(&state(50_000, 0), &cfg), 0);
}

#[test]
fn a_loan_that_would_breach_the_floor_is_refused() {
    let cfg = config(100_000, 0);
    let s = state(500_000, 0);
    assert_eq!(reserve::validate_loan(400_000, &s, &cfg), Ok(()));
    assert_eq!(
        reserve::validate_loan(400_001, &s, &cfg),
        Err(Error::ReserveFloorBreached)
    );
    assert_eq!(reserve::validate_loan(0, &s, &cfg), Err(Error::InvalidAmount));
}

#[test]
fn a_repayment_cannot_exceed_what_was_borrowed() {
    assert_eq!(reserve::validate_repayment(50_000, 100_000), Ok(()));
    assert_eq!(reserve::validate_repayment(100_000, 100_000), Ok(()));
    assert_eq!(
        reserve::validate_repayment(100_001, 100_000),
        Err(Error::RepaymentExceedsLoan)
    );
}

#[test]
fn covering_a_shortfall_may_dip_below_the_floor() {
    // The floor exists so the reserve can absorb losses. Refusing to absorb one in order
    // to protect the floor would be backwards.
    let s = state(100_000, 0);
    assert_eq!(reserve::validate_shortfall_cover(100_000, &s), Ok(()));
    assert_eq!(
        reserve::validate_shortfall_cover(100_001, &s),
        Err(Error::InsufficientReserve)
    );
}

#[test]
fn a_sweep_holds_back_the_floor_and_anything_out_on_loan() {
    let cfg = config(100_000, 0);
    // 500k held, 100k floor, 150k lent out — only 250k is truly surplus.
    let s = state(500_000, 150_000);
    assert_eq!(reserve::available_to_sweep(&s, &cfg).unwrap(), 250_000);
    assert_eq!(reserve::validate_sweep(250_000, &s, &cfg), Ok(()));
    assert_eq!(
        reserve::validate_sweep(250_001, &s, &cfg),
        Err(Error::ReserveFloorBreached)
    );
}

#[test]
fn nothing_is_sweepable_when_everything_is_committed() {
    let cfg = config(100_000, 0);
    assert_eq!(
        reserve::available_to_sweep(&state(200_000, 150_000), &cfg).unwrap(),
        0
    );
}
