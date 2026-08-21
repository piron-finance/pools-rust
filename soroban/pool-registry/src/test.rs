//! Contract tests for the registry.

use access_manager::{AccessManager, AccessManagerClient};
use piron_core::governance::roles::Role;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use super::*;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: PoolRegistryClient<'static>,
    access: AccessManagerClient<'static>,
    admin: Address,
    factory: Address,
    outsider: Address,
    asset: Address,
    spv: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let spv = Address::generate(&env);
    let operator = Address::generate(&env);
    let emergency = Address::generate(&env);
    let multisig = Address::generate(&env);
    let factory = Address::generate(&env);
    let outsider = Address::generate(&env);
    let asset = Address::generate(&env);

    let access_id = env.register(
        AccessManager,
        (
            admin.clone(),
            spv.clone(),
            operator,
            emergency,
            multisig,
        ),
    );
    let access = AccessManagerClient::new(&env, &access_id);
    access.grant_during_deployment(&admin, &(Role::Factory as u32), &factory);

    let id = env.register(PoolRegistry, (access_id,));
    let client = PoolRegistryClient::new(&env, &id);

    Fixture {
        env,
        client,
        access,
        admin,
        factory,
        outsider,
        asset,
        spv,
    }
}

#[test]
fn a_new_registry_knows_nothing() {
    let f = setup();
    assert_eq!(f.client.pool_count(), 0);
    assert!(!f.client.is_approved_asset(&f.asset));
    assert!(!f.client.is_authorized_spv(&f.spv));
}

#[test]
fn only_the_admin_approves_assets_and_spvs() {
    let f = setup();
    // Refused by the access manager, so its code (608 RoleNotHeld) is what surfaces.
    assert!(f
        .client
        .try_set_asset_approved(&f.outsider, &f.asset, &true)
        .is_err());

    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    assert!(f.client.is_approved_asset(&f.asset));

    f.client.set_spv_authorized(&f.admin, &f.spv, &true);
    assert!(f.client.is_authorized_spv(&f.spv));
}

#[test]
fn approval_can_be_withdrawn() {
    let f = setup();
    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    f.client.set_asset_approved(&f.admin, &f.asset, &false);
    assert!(!f.client.is_approved_asset(&f.asset));
}

#[test]
fn a_pool_can_only_be_registered_on_an_approved_asset() {
    let f = setup();
    let pool = Address::generate(&f.env);

    assert_eq!(
        f.client.try_register_pool(
            &f.factory,
            &pool,
            &Address::generate(&f.env),
            &Address::generate(&f.env),
            &f.asset,
            &PoolType::SingleAsset,
        ),
        Err(Ok(Error::AssetNotApproved))
    );

    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    f.client.register_pool(
        &f.factory,
        &pool,
        &Address::generate(&f.env),
        &Address::generate(&f.env),
        &f.asset,
        &PoolType::SingleAsset,
    );
    assert!(f.client.is_registered_pool(&pool));
    assert_eq!(f.client.pool_count(), 1);
}

#[test]
fn a_pool_is_registered_once() {
    let f = setup();
    let pool = Address::generate(&f.env);
    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    let manager = Address::generate(&f.env);
    let escrow = Address::generate(&f.env);

    f.client.register_pool(
        &f.factory,
        &pool,
        &manager,
        &escrow,
        &f.asset,
        &PoolType::Locked,
    );
    assert_eq!(
        f.client
            .try_register_pool(&f.factory, &pool, &manager, &escrow, &f.asset, &PoolType::Locked),
        Err(Ok(Error::AlreadyRegistered))
    );
}

#[test]
fn an_outsider_cannot_register_a_pool() {
    let f = setup();
    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    assert_eq!(
        f.client.try_register_pool(
            &f.outsider,
            &Address::generate(&f.env),
            &Address::generate(&f.env),
            &Address::generate(&f.env),
            &f.asset,
            &PoolType::StableYield,
        ),
        Err(Ok(Error::Unauthorized))
    );
    // `require_any` catches the access manager's refusal and reports its own, because
    // "neither of two roles" is a distinct condition from "not this one role".
}

#[test]
fn a_registration_records_what_the_pool_is_made_of() {
    let f = setup();
    let pool = Address::generate(&f.env);
    let manager = Address::generate(&f.env);
    let escrow = Address::generate(&f.env);
    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    f.client.register_pool(
        &f.factory,
        &pool,
        &manager,
        &escrow,
        &f.asset,
        &PoolType::StableYield,
    );

    let info = f.client.pool_info(&pool);
    assert_eq!(info.manager, manager);
    assert_eq!(info.escrow, escrow);
    assert_eq!(info.asset, f.asset);
    assert_eq!(info.pool_type, PoolType::StableYield);
    assert!(info.is_active);
}

#[test]
fn an_unregistered_pool_has_no_info() {
    let f = setup();
    assert_eq!(
        f.client.try_pool_info(&Address::generate(&f.env)),
        Err(Ok(Error::PoolNotRegistered))
    );
}

#[test]
fn deployment_needs_a_live_pool_an_approved_asset_and_a_vetted_spv() {
    let f = setup();
    let pool = Address::generate(&f.env);
    f.client.set_asset_approved(&f.admin, &f.asset, &true);
    f.client.register_pool(
        &f.factory,
        &pool,
        &Address::generate(&f.env),
        &Address::generate(&f.env),
        &f.asset,
        &PoolType::SingleAsset,
    );

    // The SPV has not been vetted yet.
    assert_eq!(
        f.client.try_require_deployable(&pool, &f.spv),
        Err(Ok(Error::SpvNotAuthorized))
    );

    f.client.set_spv_authorized(&f.admin, &f.spv, &true);
    f.client.require_deployable(&pool, &f.spv);

    // Retiring the pool closes the path.
    f.client.set_pool_active(&f.admin, &pool, &false);
    assert_eq!(
        f.client.try_require_deployable(&pool, &f.spv),
        Err(Ok(Error::PoolNotRegistered))
    );
}

#[test]
fn a_paused_protocol_blocks_registry_changes() {
    let f = setup();
    let emergency_holder = f.admin.clone();
    f.access.pause(&emergency_holder);
    assert!(f
        .client
        .try_set_asset_approved(&f.admin, &f.asset, &true)
        .is_err());
}
