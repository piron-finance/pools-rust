//! Contract tests for access control.

use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

use piron_core::governance::roles::{Role, ROLE_GRANT_DELAY};

use super::*;

const START: u64 = 1_000_000;

struct Fixture {
    env: Env,
    client: AccessManagerClient<'static>,
    admin: Address,
    spv: Address,
    operator: Address,
    emergency: Address,
    multisig: Address,
    outsider: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let admin = Address::generate(&env);
    let spv = Address::generate(&env);
    let operator = Address::generate(&env);
    let emergency = Address::generate(&env);
    let multisig = Address::generate(&env);
    let outsider = Address::generate(&env);

    let id = env.register(
        AccessManager,
        (
            admin.clone(),
            spv.clone(),
            operator.clone(),
            emergency.clone(),
            multisig.clone(),
        ),
    );
    let client = AccessManagerClient::new(&env, &id);

    Fixture {
        env,
        client,
        admin,
        spv,
        operator,
        emergency,
        multisig,
        outsider,
    }
}

// ---------------------------------------------------------------- bootstrap

#[test]
fn the_constructor_seeds_every_role() {
    let f = setup();
    assert!(f.client.has_role(&(Role::DefaultAdmin as u32), &f.admin));
    assert!(f.client.has_role(&(Role::Operator as u32), &f.admin));
    assert!(f.client.has_role(&(Role::PoolCreator as u32), &f.admin));
    assert!(f.client.has_role(&(Role::Operator as u32), &f.operator));
    assert!(f.client.has_role(&(Role::Spv as u32), &f.spv));
    assert!(f.client.has_role(&(Role::Emergency as u32), &f.emergency));
    assert!(f.client.has_role(&(Role::MultisigAdmin as u32), &f.multisig));
    assert!(!f.client.has_role(&(Role::Spv as u32), &f.outsider));
}

#[test]
#[should_panic]
fn the_emergency_wallet_cannot_be_the_admin() {
    // Otherwise it could unpause its own pause.
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    env.register(
        AccessManager,
        (
            admin.clone(),
            Address::generate(&env),
            Address::generate(&env),
            admin,
            Address::generate(&env),
        ),
    );
}

#[test]
#[should_panic]
fn the_multisig_cannot_be_the_admin() {
    // Otherwise the grant timelock is decorative.
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    env.register(
        AccessManager,
        (
            admin.clone(),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
            admin,
        ),
    );
}

// ---------------------------------------------------------------- deployment window

#[test]
fn roles_are_granted_freely_until_deployment_is_finalised() {
    let f = setup();
    assert!(!f.client.deployment_complete());

    f.client
        .grant_during_deployment(&f.admin, &(Role::Factory as u32), &f.outsider);
    assert!(f.client.has_role(&(Role::Factory as u32), &f.outsider));

    f.client.finalize_deployment(&f.admin);
    assert!(f.client.deployment_complete());

    assert_eq!(
        f.client
            .try_grant_during_deployment(&f.admin, &(Role::Spv as u32), &f.outsider),
        Err(Ok(Error::DeploymentFinalized))
    );
}

#[test]
fn only_the_admin_opens_the_deployment_window() {
    let f = setup();
    assert_eq!(
        f.client
            .try_grant_during_deployment(&f.operator, &(Role::Spv as u32), &f.outsider),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn deployment_can_only_be_finalised_once() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);
    assert_eq!(
        f.client.try_finalize_deployment(&f.admin),
        Err(Ok(Error::DeploymentFinalized))
    );
}

// ---------------------------------------------------------------- timelocked grants

#[test]
fn a_grant_after_finalisation_waits_a_day_and_needs_the_multisig() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);

    let id = f
        .client
        .propose_role_grant(&f.admin, &(Role::Spv as u32), &f.outsider);
    assert!(!f.client.has_role(&(Role::Spv as u32), &f.outsider));

    // Too early.
    assert_eq!(
        f.client.try_execute_role_grant(&f.multisig, &id),
        Err(Ok(Error::RoleDelayNotElapsed))
    );

    f.env.ledger().set_timestamp(START + ROLE_GRANT_DELAY);

    // Right time, wrong signer: the admin cannot execute its own proposal.
    assert_eq!(
        f.client.try_execute_role_grant(&f.admin, &id),
        Err(Ok(Error::Unauthorized))
    );

    f.client.execute_role_grant(&f.multisig, &id);
    assert!(f.client.has_role(&(Role::Spv as u32), &f.outsider));
}

#[test]
fn proposals_cannot_be_made_before_finalisation() {
    let f = setup();
    assert_eq!(
        f.client
            .try_propose_role_grant(&f.admin, &(Role::Spv as u32), &f.outsider),
        Err(Ok(Error::DeploymentNotFinalized))
    );
}

#[test]
fn a_proposal_executes_once() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);
    let id = f
        .client
        .propose_role_grant(&f.admin, &(Role::Spv as u32), &f.outsider);
    f.env.ledger().set_timestamp(START + ROLE_GRANT_DELAY);
    f.client.execute_role_grant(&f.multisig, &id);

    assert_eq!(
        f.client.try_execute_role_grant(&f.multisig, &id),
        Err(Ok(Error::ProposalAlreadyExecuted))
    );
}

#[test]
fn a_cancelled_proposal_cannot_be_executed() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);
    let id = f
        .client
        .propose_role_grant(&f.admin, &(Role::Spv as u32), &f.outsider);

    f.client.cancel_role_grant(&f.multisig, &id);
    f.env.ledger().set_timestamp(START + ROLE_GRANT_DELAY);

    assert_eq!(
        f.client.try_execute_role_grant(&f.multisig, &id),
        Err(Ok(Error::ProposalCancelled))
    );
    assert!(!f.client.has_role(&(Role::Spv as u32), &f.outsider));
}

#[test]
fn an_outsider_cannot_cancel_a_proposal() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);
    let id = f
        .client
        .propose_role_grant(&f.admin, &(Role::Spv as u32), &f.outsider);
    assert_eq!(
        f.client.try_cancel_role_grant(&f.outsider, &id),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn an_unknown_proposal_is_reported_not_invented() {
    let f = setup();
    f.client.finalize_deployment(&f.admin);
    assert_eq!(
        f.client.try_execute_role_grant(&f.multisig, &999),
        Err(Ok(Error::ProposalNotFound))
    );
}

// ---------------------------------------------------------------- revocation

#[test]
fn revocation_is_immediate_and_multisig_only() {
    let f = setup();
    assert_eq!(
        f.client
            .try_revoke_role(&f.admin, &(Role::Spv as u32), &f.spv),
        Err(Ok(Error::Unauthorized))
    );

    // No delay: taking a role away is the safe direction.
    f.client
        .revoke_role(&f.multisig, &(Role::Spv as u32), &f.spv);
    assert!(!f.client.has_role(&(Role::Spv as u32), &f.spv));
}

#[test]
fn revoking_a_role_nobody_holds_is_refused() {
    let f = setup();
    assert_eq!(
        f.client
            .try_revoke_role(&f.multisig, &(Role::Spv as u32), &f.outsider),
        Err(Ok(Error::RoleNotHeld))
    );
}

#[test]
fn renouncing_is_impossible_by_design() {
    let f = setup();
    assert_eq!(
        f.client.try_renounce_role(&f.admin, &(Role::DefaultAdmin as u32)),
        Err(Ok(Error::CannotRenounce))
    );
}

// ---------------------------------------------------------------- pause

#[test]
fn emergency_can_pause_but_only_the_admin_can_release() {
    let f = setup();
    assert!(!f.client.is_paused());

    f.client.pause(&f.emergency);
    assert!(f.client.is_paused());

    assert_eq!(
        f.client.try_unpause(&f.emergency),
        Err(Ok(Error::Unauthorized))
    );
    f.client.unpause(&f.admin);
    assert!(!f.client.is_paused());
}

#[test]
fn an_operator_cannot_pause() {
    let f = setup();
    assert_eq!(f.client.try_pause(&f.operator), Err(Ok(Error::Unauthorized)));
    assert_eq!(f.client.try_pause(&f.spv), Err(Ok(Error::Unauthorized)));
}

#[test]
fn a_paused_protocol_honours_no_role() {
    let f = setup();
    // The role is held, but the check other contracts call still refuses.
    assert_eq!(
        f.client.require_role(&(Role::Spv as u32), &f.spv),
        ()
    );
    f.client.pause(&f.emergency);
    assert_eq!(
        f.client.try_require_role(&(Role::Spv as u32), &f.spv),
        Err(Ok(Error::ProtocolPaused))
    );
}

#[test]
fn require_role_refuses_an_account_without_it() {
    let f = setup();
    assert_eq!(
        f.client.try_require_role(&(Role::Spv as u32), &f.outsider),
        Err(Ok(Error::RoleNotHeld))
    );
}

#[test]
fn an_unknown_role_number_is_refused() {
    let f = setup();
    assert_eq!(
        f.client
            .try_grant_during_deployment(&f.admin, &99, &f.outsider),
        Err(Ok(Error::UnknownRole))
    );
}
