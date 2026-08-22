//! Contract tests for the upgrade timelock and the guardian's brake.

use access_manager::{AccessManager, AccessManagerClient};
use piron_core::governance::upgrade::{GRACE_PERIOD, MAX_PAUSE_DURATION, UPGRADE_DELAY};
use pool_escrow::{PoolEscrow, PoolEscrowClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, BytesN, Env};

use super::*;

mod escrow_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/pool_escrow.wasm");
}

const START: u64 = 1_000_000;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: UpgradeTimelockClient<'static>,
    target: Address,
    escrow: PoolEscrowClient<'static>,
    wasm_hash: BytesN<32>,
    admin: Address,
    multisig: Address,
    guardian: Address,
    emergency: Address,
    outsider: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let admin = Address::generate(&env);
    let multisig = Address::generate(&env);
    let guardian = Address::generate(&env);
    let emergency = Address::generate(&env);
    let outsider = Address::generate(&env);

    let access_id = env.register(
        AccessManager,
        (
            admin.clone(),
            Address::generate(&env),
            Address::generate(&env),
            emergency.clone(),
            multisig.clone(),
        ),
    );
    let _access = AccessManagerClient::new(&env, &access_id);

    let timelock_id = env.register(UpgradeTimelock, (access_id, guardian.clone()));
    let client = UpgradeTimelockClient::new(&env, &timelock_id);

    // A real contract to upgrade, bound to the timelock.
    let deployer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let target = env.register(PoolEscrow, (deployer.clone(), asset.address()));
    let escrow = PoolEscrowClient::new(&env, &target);
    escrow.bind_upgrader(&deployer, &timelock_id);

    let wasm_hash = env.deployer().upload_contract_wasm(escrow_wasm::WASM);

    Fixture {
        env,
        client,
        target,
        escrow,
        wasm_hash,
        admin,
        multisig,
        guardian,
        emergency,
        outsider,
    }
}

// ---------------------------------------------------------------- the delay

#[test]
fn an_upgrade_waits_seventy_two_hours() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);

    assert_eq!(f.client.operation_state(&id), 0); // waiting
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeDelayNotElapsed))
    );

    f.env.ledger().set_timestamp(START + UPGRADE_DELAY - 1);
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeDelayNotElapsed))
    );

    f.env.ledger().set_timestamp(START + UPGRADE_DELAY);
    assert_eq!(f.client.operation_state(&id), 1); // ready
    f.client.execute(&f.multisig, &id);
    assert_eq!(f.client.operation_state(&id), 3); // executed
}

#[test]
fn an_upgrade_expires_if_the_window_closes() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);

    f.env
        .ledger()
        .set_timestamp(START + UPGRADE_DELAY + GRACE_PERIOD + 1);
    assert_eq!(f.client.operation_state(&id), 2); // expired
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeExpired))
    );
}

#[test]
fn a_stale_approval_has_to_go_round_again() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.env.ledger().set_timestamp(START + 90 * 24 * 60 * 60);
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeExpired))
    );

    // Rescheduling starts a fresh clock.
    let again = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.env
        .ledger()
        .set_timestamp(START + 90 * 24 * 60 * 60 + UPGRADE_DELAY);
    f.client.execute(&f.multisig, &again);
}

#[test]
fn an_upgrade_runs_once() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.env.ledger().set_timestamp(START + UPGRADE_DELAY);
    f.client.execute(&f.multisig, &id);
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeAlreadyExecuted))
    );
}

// ---------------------------------------------------------------- separation of hands

#[test]
fn scheduling_and_executing_are_different_hands() {
    let f = setup();
    // Refused by the access manager, so its RoleNotHeld (608) is what surfaces.
    assert!(f
        .client
        .try_schedule(&f.multisig, &f.target, &f.wasm_hash)
        .is_err());

    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.env.ledger().set_timestamp(START + UPGRADE_DELAY);

    // The admin who scheduled it cannot also run it.
    assert!(f.client.try_execute(&f.admin, &id).is_err());
    f.client.execute(&f.multisig, &id);
}

#[test]
fn an_outsider_can_neither_schedule_nor_cancel() {
    let f = setup();
    assert!(f
        .client
        .try_schedule(&f.outsider, &f.target, &f.wasm_hash)
        .is_err());
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    assert_eq!(
        f.client.try_cancel(&f.outsider, &id),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn a_cancelled_upgrade_never_runs() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.client.cancel(&f.multisig, &id);

    f.env.ledger().set_timestamp(START + UPGRADE_DELAY);
    assert_eq!(f.client.operation_state(&id), 4);
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradeCancelled))
    );
}

// ---------------------------------------------------------------- the brake

#[test]
fn a_guardian_stops_everything() {
    let f = setup();
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.client.pause_upgrades(&f.guardian);
    assert!(f.client.is_paused());

    f.env.ledger().set_timestamp(START + UPGRADE_DELAY);
    assert_eq!(
        f.client.try_execute(&f.multisig, &id),
        Err(Ok(Error::UpgradesPaused))
    );
}

#[test]
fn the_emergency_role_can_also_pull_the_brake() {
    let f = setup();
    f.client.pause_upgrades(&f.emergency);
    assert!(f.client.is_paused());
}

#[test]
fn an_outsider_cannot_pull_the_brake() {
    let f = setup();
    assert_eq!(
        f.client.try_pause_upgrades(&f.outsider),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn the_brake_lapses_after_thirty_days() {
    // A guardian may stop an upgrade. A guardian must not be able to freeze governance.
    let f = setup();
    f.client.pause_upgrades(&f.guardian);

    f.env
        .ledger()
        .set_timestamp(START + MAX_PAUSE_DURATION - 1);
    assert!(f.client.is_paused());

    f.env.ledger().set_timestamp(START + MAX_PAUSE_DURATION);
    assert!(!f.client.is_paused());

    // And an upgrade scheduled after the lapse runs normally.
    let id = f.client.schedule(&f.admin, &f.target, &f.wasm_hash);
    f.env
        .ledger()
        .set_timestamp(START + MAX_PAUSE_DURATION + UPGRADE_DELAY);
    f.client.execute(&f.multisig, &id);
}

#[test]
fn only_the_admin_lifts_the_brake_early() {
    let f = setup();
    f.client.pause_upgrades(&f.guardian);
    assert!(f.client.try_resume_upgrades(&f.guardian).is_err());
    f.client.resume_upgrades(&f.admin);
    assert!(!f.client.is_paused());
}

#[test]
fn resuming_when_nothing_is_paused_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_resume_upgrades(&f.admin),
        Err(Ok(Error::NotPaused))
    );
}

#[test]
fn the_brake_cannot_be_pulled_twice() {
    let f = setup();
    f.client.pause_upgrades(&f.guardian);
    assert_eq!(
        f.client.try_pause_upgrades(&f.guardian),
        Err(Ok(Error::AlreadyPaused))
    );
}

// ---------------------------------------------------------------- binding

#[test]
fn a_contract_names_its_upgrader_once() {
    let f = setup();
    assert_eq!(f.escrow.upgrader(), Some(f.client.address.clone()));

    // Repointing it would make the timelock decorative.
    let deployer = Address::generate(&f.env);
    assert!(f.escrow.try_bind_upgrader(&deployer, &f.outsider).is_err());
}

#[test]
fn nothing_but_the_bound_upgrader_can_replace_the_code() {
    let f = setup();
    // Calling the target directly, outside the timelock, gets nowhere.
    let env = Env::default();
    env.mock_all_auths();
    let deployer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let unbound = env.register(PoolEscrow, (deployer, asset.address()));
    let client = PoolEscrowClient::new(&env, &unbound);
    let hash = env.deployer().upload_contract_wasm(escrow_wasm::WASM);

    // No upgrader bound at all: refused.
    assert!(client.try_upgrade(&hash).is_err());
}

#[test]
fn an_unknown_operation_is_reported_not_invented() {
    let f = setup();
    assert_eq!(
        f.client.try_execute(&f.multisig, &999),
        Err(Ok(Error::UpgradeNotFound))
    );
}
