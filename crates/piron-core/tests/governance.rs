//! Tests for the shared governance layer: roles, the grant timelock, and fee splits.

use piron_core::error::Error;
use piron_core::governance::{fees, roles};
use piron_core::BASIS_POINTS;

const NOW: u64 = 1_000_000;

// ---------------------------------------------------------------- roles

#[test]
fn the_seven_roles_round_trip_their_wire_values() {
    for value in 0..roles::Role::COUNT {
        let role = roles::Role::from_u32(value).unwrap();
        assert_eq!(role as u32, value);
    }
    assert_eq!(roles::Role::from_u32(7), Err(Error::UnknownRole));
}

#[test]
fn pausing_is_wider_than_unpausing() {
    // Emergency can hit the brake; only the admin can release it.
    assert!(roles::Role::Emergency.can_pause());
    assert!(!roles::Role::Emergency.can_unpause());
    assert!(roles::Role::DefaultAdmin.can_pause());
    assert!(roles::Role::DefaultAdmin.can_unpause());
    assert!(!roles::Role::Operator.can_pause());
    assert!(!roles::Role::Spv.can_pause());
}

#[test]
fn a_role_grant_waits_out_the_delay() {
    let proposal = roles::RoleProposal::new(roles::Role::Spv, NOW);
    assert_eq!(proposal.executable_at(), NOW + roles::ROLE_GRANT_DELAY);

    assert_eq!(
        roles::validate_proposal_execution(&proposal, NOW),
        Err(Error::RoleDelayNotElapsed)
    );
    assert_eq!(
        roles::validate_proposal_execution(&proposal, proposal.executable_at() - 1),
        Err(Error::RoleDelayNotElapsed)
    );
    assert_eq!(
        roles::validate_proposal_execution(&proposal, proposal.executable_at()),
        Ok(())
    );
}

#[test]
fn a_grant_cannot_be_executed_twice_or_after_cancellation() {
    let mut proposal = roles::RoleProposal::new(roles::Role::Spv, NOW);
    let ready = proposal.executable_at();

    proposal.state = roles::ProposalState::Executed;
    assert_eq!(
        roles::validate_proposal_execution(&proposal, ready),
        Err(Error::ProposalAlreadyExecuted)
    );

    proposal.state = roles::ProposalState::Cancelled;
    assert_eq!(
        roles::validate_proposal_execution(&proposal, ready),
        Err(Error::ProposalCancelled)
    );
}

#[test]
fn the_delay_is_a_full_day() {
    assert_eq!(roles::ROLE_GRANT_DELAY, 24 * 60 * 60);
}

// ---------------------------------------------------------------- fees

fn split(treasury: u32, reserve: u32, ops: u32) -> fees::FeeSplit {
    fees::FeeSplit {
        treasury_bps: treasury,
        reserve_bps: reserve,
        ops_bps: ops,
        active: true,
    }
}

#[test]
fn a_split_must_account_for_every_basis_point() {
    assert_eq!(fees::validate_split(&split(5_000, 3_000, 2_000)), Ok(()));
    assert_eq!(
        fees::validate_split(&split(5_000, 3_000, 1_999)),
        Err(Error::InvalidSplitRatio)
    );
    assert_eq!(
        fees::validate_split(&split(5_000, 3_000, 2_001)),
        Err(Error::InvalidSplitRatio)
    );
}

#[test]
fn an_inactive_fee_type_collects_nothing() {
    let mut s = split(5_000, 3_000, 2_000);
    s.active = false;
    assert_eq!(fees::validate_split(&s), Err(Error::FeeTypeInactive));
    assert_eq!(fees::split_fee(1_000, &s), Err(Error::FeeTypeInactive));
}

#[test]
fn a_fee_divides_by_the_configured_shares() {
    let allocation = fees::split_fee(10_000, &split(5_000, 3_000, 2_000)).unwrap();
    assert_eq!(allocation.to_treasury, 5_000);
    assert_eq!(allocation.to_reserve, 3_000);
    assert_eq!(allocation.to_ops, 2_000);
}

#[test]
fn the_parts_always_sum_back_to_the_whole() {
    // Ops absorbs the truncation, so no dust is created or lost on any runtime.
    for amount in [1, 7, 999, 10_001, 1_234_567] {
        let allocation = fees::split_fee(amount, &split(3_333, 3_333, 3_334)).unwrap();
        assert_eq!(allocation.total().unwrap(), amount, "amount {amount}");
    }
}

#[test]
fn a_single_unit_fee_goes_somewhere_rather_than_vanishing() {
    let allocation = fees::split_fee(1, &split(5_000, 3_000, 2_000)).unwrap();
    // Both rounded shares truncate to zero; ops takes the unit.
    assert_eq!(allocation.to_treasury, 0);
    assert_eq!(allocation.to_reserve, 0);
    assert_eq!(allocation.to_ops, 1);
}

#[test]
fn a_zero_fee_allocates_nothing() {
    let allocation = fees::split_fee(0, &split(5_000, 3_000, 2_000)).unwrap();
    assert_eq!(allocation.total().unwrap(), 0);
}

#[test]
fn the_default_split_sends_everything_to_the_treasury() {
    let allocation = fees::split_fee(10_000, &fees::FeeSplit::all_treasury()).unwrap();
    assert_eq!(allocation.to_treasury, 10_000);
    assert_eq!(allocation.to_ops, 0);
    assert_eq!(fees::FeeSplit::all_treasury().treasury_bps, BASIS_POINTS);
}

#[test]
fn the_seven_fee_types_round_trip_their_wire_values() {
    for value in 0..fees::FeeType::COUNT {
        assert_eq!(fees::FeeType::from_u32(value).unwrap() as u32, value);
    }
    assert_eq!(fees::FeeType::from_u32(7), Err(Error::UnknownFeeType));
}

#[test]
fn a_negative_fee_is_refused() {
    assert_eq!(
        fees::split_fee(-1, &split(5_000, 3_000, 2_000)),
        Err(Error::NegativeAmount)
    );
}
