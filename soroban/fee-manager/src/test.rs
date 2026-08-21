//! Contract tests for fee collection and distribution.

use access_manager::AccessManager;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use super::*;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: FeeManagerClient<'static>,
    token: TokenClient<'static>,
    asset: Address,
    admin: Address,
    collector: Address,
    outsider: Address,
    treasury: Address,
    reserve: Address,
    ops: Address,
    pool: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let collector = Address::generate(&env);
    let outsider = Address::generate(&env);
    let treasury = Address::generate(&env);
    let reserve = Address::generate(&env);
    let ops = Address::generate(&env);
    let pool = Address::generate(&env);

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
    let issuer = Address::generate(&env);
    let asset_contract = env.register_stellar_asset_contract_v2(issuer);
    let asset = asset_contract.address();
    let token = TokenClient::new(&env, &asset);
    StellarAssetClient::new(&env, &asset).mint(&collector, &1_000_000);

    let id = env.register(
        FeeManager,
        (
            access_id,
            Destinations {
                treasury: treasury.clone(),
                reserve: reserve.clone(),
                ops: ops.clone(),
            },
        ),
    );
    let client = FeeManagerClient::new(&env, &id);
    client.set_collector(&admin, &collector, &true);

    Fixture {
        env,
        client,
        token,
        asset,
        admin,
        collector,
        outsider,
        treasury,
        reserve,
        ops,
        pool,
    }
}

fn split(t: u32, r: u32, o: u32) -> StoredSplit {
    StoredSplit {
        treasury_bps: t,
        reserve_bps: r,
        ops_bps: o,
        active: true,
    }
}

#[test]
fn an_unconfigured_fee_type_defaults_to_the_treasury() {
    let f = setup();
    let s = f.client.split_for(&(FeeType::Withdrawal as u32));
    assert_eq!(s.treasury_bps, 10_000);
}

#[test]
fn a_split_must_total_a_hundred_percent() {
    let f = setup();
    assert_eq!(
        f.client
            .try_set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(5_000, 3_000, 1_000)),
        Err(Ok(Error::InvalidSplitRatio))
    );
    f.client
        .set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(5_000, 3_000, 2_000));
    assert_eq!(
        f.client.split_for(&(FeeType::Withdrawal as u32)).reserve_bps,
        3_000
    );
}

#[test]
fn only_the_admin_configures_splits_and_collectors() {
    let f = setup();
    // The refusal comes from the access manager, so it surfaces as that contract's
    // RoleNotHeld (608) rather than this one's Unauthorized. One taxonomy, one number,
    // whichever contract raised it.
    assert!(f
        .client
        .try_set_split(&f.outsider, &(FeeType::Withdrawal as u32), &split(10_000, 0, 0))
        .is_err());
    assert!(f
        .client
        .try_set_collector(&f.outsider, &f.outsider, &true)
        .is_err());
}

#[test]
fn only_an_authorised_collector_can_pay_fees_in() {
    let f = setup();
    assert!(!f.client.is_collector(&f.outsider));
    assert_eq!(
        f.client.try_collect_fee(
            &f.outsider,
            &f.pool,
            &f.asset,
            &1_000,
            &(FeeType::Withdrawal as u32)
        ),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn collecting_a_fee_moves_the_tokens_and_books_the_split() {
    let f = setup();
    f.client
        .set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(5_000, 3_000, 2_000));

    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &10_000,
        &(FeeType::Withdrawal as u32),
    );

    assert_eq!(f.token.balance(&f.collector), 990_000);
    let acc = f.client.accounting_for(&f.asset);
    assert_eq!(acc.total_collected, 10_000);
    assert_eq!(acc.pending_treasury, 5_000);
    assert_eq!(acc.pending_reserve, 3_000);
    assert_eq!(acc.pending_ops, 2_000);
}

#[test]
fn fees_accumulate_across_collections() {
    let f = setup();
    f.client
        .set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(5_000, 3_000, 2_000));
    for _ in 0..3 {
        f.client.collect_fee(
            &f.collector,
            &f.pool,
            &f.asset,
            &10_000,
            &(FeeType::Withdrawal as u32),
        );
    }
    assert_eq!(f.client.accounting_for(&f.asset).total_collected, 30_000);
    assert_eq!(f.client.accounting_for(&f.asset).pending_treasury, 15_000);
}

#[test]
fn each_fee_type_can_split_differently() {
    let f = setup();
    f.client
        .set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(10_000, 0, 0));
    f.client
        .set_split(&f.admin, &(FeeType::EarlyExitPenalty as u32), &split(0, 10_000, 0));

    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &1_000,
        &(FeeType::Withdrawal as u32),
    );
    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &1_000,
        &(FeeType::EarlyExitPenalty as u32),
    );

    let acc = f.client.accounting_for(&f.asset);
    // Penalties fund the reserve; withdrawal fees fund the treasury.
    assert_eq!(acc.pending_treasury, 1_000);
    assert_eq!(acc.pending_reserve, 1_000);
}

#[test]
fn distribution_pays_all_three_destinations_and_clears_pending() {
    let f = setup();
    f.client
        .set_split(&f.admin, &(FeeType::Withdrawal as u32), &split(5_000, 3_000, 2_000));
    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &10_000,
        &(FeeType::Withdrawal as u32),
    );

    assert_eq!(f.client.distribute(&f.admin, &f.asset), 10_000);
    assert_eq!(f.token.balance(&f.treasury), 5_000);
    assert_eq!(f.token.balance(&f.reserve), 3_000);
    assert_eq!(f.token.balance(&f.ops), 2_000);

    let acc = f.client.accounting_for(&f.asset);
    assert_eq!(acc.pending_treasury, 0);
    assert_eq!(acc.total_distributed, 10_000);
}

#[test]
fn distributing_nothing_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_distribute(&f.admin, &f.asset),
        Err(Ok(Error::NothingToDistribute))
    );
}

#[test]
fn distribution_can_be_paused() {
    let f = setup();
    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &1_000,
        &(FeeType::Withdrawal as u32),
    );
    f.client.set_distribution_paused(&f.admin, &true);
    assert_eq!(
        f.client.try_distribute(&f.admin, &f.asset),
        Err(Ok(Error::DistributionPaused))
    );
    f.client.set_distribution_paused(&f.admin, &false);
    assert_eq!(f.client.distribute(&f.admin, &f.asset), 1_000);
}

#[test]
fn a_zero_fee_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_collect_fee(
            &f.collector,
            &f.pool,
            &f.asset,
            &0,
            &(FeeType::Withdrawal as u32)
        ),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn an_unknown_fee_type_is_refused() {
    let f = setup();
    assert_eq!(
        f.client
            .try_collect_fee(&f.collector, &f.pool, &f.asset, &1_000, &99),
        Err(Ok(Error::UnknownFeeType))
    );
}

#[test]
fn everything_collected_is_everything_distributed() {
    let f = setup();
    // An awkward split that truncates, on an awkward amount.
    f.client
        .set_split(&f.admin, &(FeeType::Spread as u32), &split(3_333, 3_333, 3_334));
    f.client.collect_fee(
        &f.collector,
        &f.pool,
        &f.asset,
        &1_001,
        &(FeeType::Spread as u32),
    );

    assert_eq!(f.client.distribute(&f.admin, &f.asset), 1_001);
    let paid = f.token.balance(&f.treasury) + f.token.balance(&f.reserve) + f.token.balance(&f.ops);
    assert_eq!(paid, 1_001);
    // Nothing stuck in the contract.
    assert_eq!(f.token.balance(&f.collector), 1_000_000 - 1_001);
}
