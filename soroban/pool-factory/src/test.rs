//! Contract tests for the factory.
//!
//! These need the wasm built first, because a factory deploys by hash and there is no
//! honest way to fake that: `make soroban-build` then `cargo test`. `make soroban-test`
//! does both in order.

use access_manager::{AccessManager, AccessManagerClient};
use piron_core::governance::roles::Role;
use pool_escrow::PoolEscrowClient;
use pool_registry::{PoolRegistry, PoolRegistryClient};
use single_asset_manager::SingleAssetManagerClient;
use soroban_sdk::testutils::{Address as _, BytesN as _, Ledger};
use soroban_sdk::token::StellarAssetClient;
use soroban_sdk::{Address, BytesN, Env, Vec};

use super::*;

mod escrow_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/pool_escrow.wasm");
}
mod single_asset_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/single_asset_manager.wasm"
    );
}
mod locked_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/locked_pool_manager.wasm"
    );
}
mod stable_yield_wasm {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/stable_yield_manager.wasm"
    );
}

const EPOCH_END: u64 = 1_000;
const MATURITY: u64 = 11_000;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    factory: PoolFactoryClient<'static>,
    registry: PoolRegistryClient<'static>,
    asset: Address,
    admin: Address,
    spv: Address,
    operator: Address,
    emergency: Address,
    treasury: Address,
    outsider: Address,
    investor: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100);

    let admin = Address::generate(&env);
    let spv = Address::generate(&env);
    let operator = Address::generate(&env);
    let emergency = Address::generate(&env);
    let multisig = Address::generate(&env);
    let treasury = Address::generate(&env);
    let outsider = Address::generate(&env);
    let investor = Address::generate(&env);

    let access_id = env.register(
        AccessManager,
        (
            admin.clone(),
            spv.clone(),
            operator.clone(),
            emergency.clone(),
            multisig,
        ),
    );
    let access = AccessManagerClient::new(&env, &access_id);

    let registry_id = env.register(PoolRegistry, (access_id.clone(),));
    let registry = PoolRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let asset_contract = env.register_stellar_asset_contract_v2(issuer);
    let asset = asset_contract.address();
    StellarAssetClient::new(&env, &asset).mint(&investor, &10_000_000);

    let escrow_hash = env.deployer().upload_contract_wasm(escrow_wasm::WASM);
    let factory_id = env.register(
        PoolFactory,
        (access_id, registry_id, escrow_hash),
    );
    let factory = PoolFactoryClient::new(&env, &factory_id);

    // The factory registers the pools it creates, so it needs the role to do so.
    access.grant_during_deployment(&admin, &(Role::Factory as u32), &factory_id);
    registry.set_asset_approved(&admin, &asset, &true);

    factory.set_manager_wasm(
        &admin,
        &PoolType::SingleAsset,
        &env.deployer().upload_contract_wasm(single_asset_wasm::WASM),
    );
    factory.set_manager_wasm(
        &admin,
        &PoolType::Locked,
        &env.deployer().upload_contract_wasm(locked_wasm::WASM),
    );
    factory.set_manager_wasm(
        &admin,
        &PoolType::StableYield,
        &env.deployer().upload_contract_wasm(stable_yield_wasm::WASM),
    );

    Fixture {
        env,
        factory,
        registry,
        asset,
        admin,
        spv,
        operator,
        emergency,
        treasury,
        outsider,
        investor,
    }
}

fn pool_roles(f: &Fixture) -> Roles {
    Roles {
        admin: f.admin.clone(),
        operator: f.operator.clone(),
        spv: f.spv.clone(),
        emergency: f.emergency.clone(),
        treasury: f.treasury.clone(),
    }
}

fn single_asset_config(env: &Env, asset_target: i128) -> StoredConfig {
    StoredConfig {
        instrument_type: 0,
        face_value: 0,
        purchase_price: asset_target,
        target_raise: asset_target,
        epoch_end_time: EPOCH_END,
        maturity_date: MATURITY,
        coupon_dates: Vec::new(env),
        coupon_rates: Vec::new(env),
        discount_rate: 500,
        minimum_funding_threshold: 8_000,
        min_investment: 1_000,
        withdrawal_fee_bps: 0,
    }
}

fn salt(env: &Env, byte: u8) -> BytesN<32> {
    let mut raw = [0u8; 32];
    raw[31] = byte;
    let _ = BytesN::<32>::random(env);
    BytesN::from_array(env, &raw)
}

// ---------------------------------------------------------------- determinism

#[test]
fn an_address_can_be_known_before_the_pool_exists() {
    let f = setup();
    let s = salt(&f.env, 1);

    let predicted = f.factory.predict(&s);
    assert!(!f.factory.is_salt_used(&s));

    let created = f.factory.create_single_asset_pool(
        &f.admin,
        &s,
        &single_asset_config(&f.env, 1_000_000),
        &pool_roles(&f),
        &f.asset,
    );

    assert_eq!(created.manager, predicted.manager);
    assert_eq!(created.escrow, predicted.escrow);
}

#[test]
fn different_salts_give_different_pools() {
    let f = setup();
    let a = f.factory.predict(&salt(&f.env, 1));
    let b = f.factory.predict(&salt(&f.env, 2));
    assert_ne!(a.manager, b.manager);
    assert_ne!(a.escrow, b.escrow);
}

#[test]
fn a_pool_and_its_escrow_never_collide() {
    let f = setup();
    let p = f.factory.predict(&salt(&f.env, 1));
    assert_ne!(p.manager, p.escrow);
}

#[test]
fn a_salt_is_spent_once() {
    let f = setup();
    let s = salt(&f.env, 1);
    f.factory.create_single_asset_pool(
        &f.admin,
        &s,
        &single_asset_config(&f.env, 1_000_000),
        &pool_roles(&f),
        &f.asset,
    );
    assert!(f.factory.is_salt_used(&s));

    assert_eq!(
        f.factory.try_create_single_asset_pool(
            &f.admin,
            &s,
            &single_asset_config(&f.env, 1_000_000),
            &pool_roles(&f),
            &f.asset,
        ),
        Err(Ok(Error::SaltAlreadyUsed))
    );
}

// ---------------------------------------------------------------- wiring

#[test]
fn a_created_pool_is_wired_and_registered_in_one_call() {
    let f = setup();
    let created = f.factory.create_single_asset_pool(
        &f.admin,
        &salt(&f.env, 1),
        &single_asset_config(&f.env, 1_000_000),
        &pool_roles(&f),
        &f.asset,
    );

    // The escrow obeys the manager and nobody else — no window where it sits unbound.
    let escrow = PoolEscrowClient::new(&f.env, &created.escrow);
    assert_eq!(escrow.manager(), created.manager);

    // The registry knows about it, with the right shape.
    assert!(f.registry.is_registered_pool(&created.manager));
    let info = f.registry.pool_info(&created.manager);
    assert_eq!(info.escrow, created.escrow);
    assert_eq!(info.asset, f.asset);
    assert_eq!(info.pool_type, PoolType::SingleAsset);

    assert_eq!(f.factory.pool_count(), 1);
}

#[test]
fn a_pool_from_the_factory_actually_works() {
    let f = setup();
    let created = f.factory.create_single_asset_pool(
        &f.admin,
        &salt(&f.env, 1),
        &single_asset_config(&f.env, 1_000_000),
        &pool_roles(&f),
        &f.asset,
    );

    let manager = SingleAssetManagerClient::new(&f.env, &created.manager);
    let escrow = PoolEscrowClient::new(&f.env, &created.escrow);

    assert_eq!(manager.deposit(&f.investor, &400_000), 400_000);
    assert_eq!(escrow.balance(), 400_000);
    assert_eq!(manager.total_supply(), 400_000);
}

#[test]
fn the_factory_builds_all_three_pool_types() {
    let f = setup();

    let single = f.factory.create_single_asset_pool(
        &f.admin,
        &salt(&f.env, 1),
        &single_asset_config(&f.env, 1_000_000),
        &pool_roles(&f),
        &f.asset,
    );
    let locked = f.factory.create_locked_pool(
        &f.admin,
        &salt(&f.env, 2),
        &pool_roles(&f),
        &f.asset,
        &1_000,
    );
    let stable = f.factory.create_stable_yield_pool(
        &f.admin,
        &salt(&f.env, 3),
        &pool_roles(&f),
        &f.asset,
        &StableYieldConfig {
            min_investment: 1_000,
            holding_period: 604_800,
            withdrawal_fee_bps: 50,
            min_absolute_reserve: 0,
            reserve_ratio_bps: 2_000,
            is_active: true,
        },
    );

    assert_eq!(f.factory.pool_count(), 3);
    assert_eq!(
        f.registry.pool_info(&single.manager).pool_type,
        PoolType::SingleAsset
    );
    assert_eq!(
        f.registry.pool_info(&locked.manager).pool_type,
        PoolType::Locked
    );
    assert_eq!(
        f.registry.pool_info(&stable.manager).pool_type,
        PoolType::StableYield
    );
}

// ---------------------------------------------------------------- authorisation

#[test]
fn an_outsider_cannot_create_a_pool() {
    let f = setup();
    assert_eq!(
        f.factory.try_create_single_asset_pool(
            &f.outsider,
            &salt(&f.env, 1),
            &single_asset_config(&f.env, 1_000_000),
            &pool_roles(&f),
            &f.asset,
        ),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn only_the_admin_points_a_pool_kind_at_new_code() {
    let f = setup();
    let hash = f.env.deployer().upload_contract_wasm(escrow_wasm::WASM);
    assert!(f
        .factory
        .try_set_manager_wasm(&f.outsider, &PoolType::SingleAsset, &hash)
        .is_err());
}

#[test]
fn a_pool_kind_with_no_code_registered_cannot_be_built() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let access_id = env.register(
        AccessManager,
        (
            admin.clone(),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    let registry_id = env.register(PoolRegistry, (access_id.clone(),));
    let escrow_hash = env.deployer().upload_contract_wasm(escrow_wasm::WASM);
    let factory_id = env.register(PoolFactory, (access_id, registry_id, escrow_hash));
    let factory = PoolFactoryClient::new(&env, &factory_id);

    assert_eq!(
        factory.try_manager_wasm_for(&PoolType::SingleAsset),
        Err(Ok(Error::WasmNotSet))
    );
}

#[test]
fn a_pool_cannot_be_created_on_an_unapproved_asset() {
    let f = setup();
    // The registry refuses the registration, so the whole creation reverts.
    f.registry.set_asset_approved(&f.admin, &f.asset, &false);
    assert!(f
        .factory
        .try_create_single_asset_pool(
            &f.admin,
            &salt(&f.env, 1),
            &single_asset_config(&f.env, 1_000_000),
            &pool_roles(&f),
            &f.asset,
        )
        .is_err());
    // Nothing was left half-built.
    assert_eq!(f.factory.pool_count(), 0);
    assert!(!f.factory.is_salt_used(&salt(&f.env, 1)));
}
