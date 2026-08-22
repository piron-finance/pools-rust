//! Contract tests for the protocol reserve.

use access_manager::AccessManager;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use super::*;

#[allow(dead_code)]
struct Fixture {
    env: Env,
    client: YieldReserveClient<'static>,
    token: TokenClient<'static>,
    asset: Address,
    admin: Address,
    operator: Address,
    treasury: Address,
    manager: Address,
    pool: Address,
    outsider: Address,
}

fn setup_with(floor: i128, treasury_bps: u32) -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let operator = Address::generate(&env);
    let treasury = Address::generate(&env);
    let manager = Address::generate(&env);
    let pool = Address::generate(&env);
    let outsider = Address::generate(&env);

    let access_id = env.register(
        AccessManager,
        (
            admin.clone(),
            Address::generate(&env),
            operator.clone(),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );

    let asset_contract = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let asset = asset_contract.address();
    let token = TokenClient::new(&env, &asset);
    StellarAssetClient::new(&env, &asset).mint(&manager, &10_000_000);

    let id = env.register(
        YieldReserve,
        (
            access_id,
            asset.clone(),
            treasury.clone(),
            Config {
                min_floor: floor,
                treasury_split_bps: treasury_bps,
            },
        ),
    );
    let client = YieldReserveClient::new(&env, &id);
    client.set_source(&admin, &manager, &true);

    Fixture {
        env,
        client,
        token,
        asset,
        admin,
        operator,
        treasury,
        manager,
        pool,
        outsider,
    }
}

fn setup() -> Fixture {
    setup_with(100_000, 3_000)
}

/// Fills the reserve with `retained` net of the treasury split.
fn fund(f: &Fixture, gross: i128) {
    f.client.receive_yield(&f.manager, &gross);
}

// ---------------------------------------------------------------- inflow

#[test]
fn incoming_yield_pays_the_treasury_and_retains_the_rest() {
    let f = setup();
    fund(&f, 1_000_000);

    // 30% out, 70% held.
    assert_eq!(f.token.balance(&f.treasury), 300_000);
    let stats = f.client.stats_of();
    assert_eq!(stats.balance, 700_000);
    assert_eq!(stats.total_yield_received, 1_000_000);
}

#[test]
fn only_an_authorised_source_can_pay_in() {
    let f = setup();
    assert_eq!(
        f.client.try_receive_yield(&f.outsider, &1_000),
        Err(Ok(Error::Unauthorized))
    );
    assert!(!f.client.is_source(&f.outsider));
}

#[test]
fn a_zero_or_negative_contribution_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_receive_yield(&f.manager, &0),
        Err(Ok(Error::InvalidAmount))
    );
}

// ---------------------------------------------------------------- lending

#[test]
fn the_reserve_lends_only_above_its_floor() {
    let f = setup();
    fund(&f, 1_000_000); // 700,000 retained, 100,000 floor

    assert_eq!(f.client.available_for_loan(), 600_000);
    assert_eq!(
        f.client
            .try_loan_to_pool(&f.operator, &f.pool, &f.pool, &600_001),
        Err(Ok(Error::ReserveFloorBreached))
    );

    f.client.loan_to_pool(&f.operator, &f.pool, &f.pool, &600_000);
    assert_eq!(f.token.balance(&f.pool), 600_000);
    assert_eq!(f.client.loan_of(&f.pool), 600_000);
    assert_eq!(f.client.available_for_loan(), 0);
}

#[test]
fn only_an_operator_lends() {
    let f = setup();
    fund(&f, 1_000_000);
    assert!(f
        .client
        .try_loan_to_pool(&f.outsider, &f.pool, &f.pool, &1_000)
        .is_err());
}

#[test]
fn a_repayment_clears_the_loan_and_restores_the_balance() {
    let f = setup();
    fund(&f, 1_000_000);
    f.client.loan_to_pool(&f.operator, &f.pool, &f.manager, &500_000);
    assert_eq!(f.client.stats_of().balance, 200_000);

    f.client.repay_loan(&f.manager, &f.pool, &200_000);
    assert_eq!(f.client.loan_of(&f.pool), 300_000);

    f.client.repay_loan(&f.manager, &f.pool, &300_000);
    assert_eq!(f.client.loan_of(&f.pool), 0);
    assert_eq!(f.client.stats_of().balance, 700_000);
    assert_eq!(f.client.stats_of().total_loaned, 0);
}

#[test]
fn a_pool_cannot_repay_more_than_it_borrowed() {
    let f = setup();
    fund(&f, 1_000_000);
    f.client.loan_to_pool(&f.operator, &f.pool, &f.manager, &100_000);
    assert_eq!(
        f.client.try_repay_loan(&f.manager, &f.pool, &100_001),
        Err(Ok(Error::RepaymentExceedsLoan))
    );
}

#[test]
fn loans_are_tracked_per_pool() {
    let f = setup();
    fund(&f, 1_000_000);
    let other = Address::generate(&f.env);
    f.client.loan_to_pool(&f.operator, &f.pool, &f.manager, &100_000);
    f.client.loan_to_pool(&f.operator, &other, &f.manager, &200_000);

    assert_eq!(f.client.loan_of(&f.pool), 100_000);
    assert_eq!(f.client.loan_of(&other), 200_000);
    assert_eq!(f.client.stats_of().total_loaned, 300_000);
}

// ---------------------------------------------------------------- loss

#[test]
fn covering_a_shortfall_may_go_below_the_floor() {
    // The floor exists so losses can be absorbed. Refusing to absorb one to protect it
    // would be backwards.
    let f = setup();
    fund(&f, 1_000_000);
    let investor = Address::generate(&f.env);

    f.client
        .cover_shortfall(&f.operator, &f.pool, &investor, &650_000);
    assert_eq!(f.token.balance(&investor), 650_000);

    let stats = f.client.stats_of();
    assert_eq!(stats.balance, 50_000); // below the 100,000 floor, deliberately
    assert_eq!(stats.total_shortfall_covered, 650_000);
    assert_eq!(f.client.available_for_loan(), 0);
}

#[test]
fn a_shortfall_larger_than_the_reserve_is_refused() {
    let f = setup();
    fund(&f, 1_000_000);
    assert_eq!(
        f.client
            .try_cover_shortfall(&f.operator, &f.pool, &f.pool, &700_001),
        Err(Ok(Error::InsufficientReserve))
    );
}

// ---------------------------------------------------------------- outflow

#[test]
fn a_sweep_holds_back_the_floor_and_outstanding_loans() {
    let f = setup();
    fund(&f, 1_000_000); // 700,000 retained
    f.client.loan_to_pool(&f.operator, &f.pool, &f.manager, &200_000);

    // 500,000 held, 100,000 floor, 200,000 still owed to the reserve: 200,000 is surplus.
    assert_eq!(f.client.available_to_sweep(), 200_000);

    let before = f.token.balance(&f.treasury);
    assert_eq!(f.client.sweep_to_treasury(&f.operator), 200_000);
    assert_eq!(f.token.balance(&f.treasury), before + 200_000);
    assert_eq!(f.client.stats_of().balance, 300_000);
}

#[test]
fn there_is_nothing_to_sweep_when_everything_is_committed() {
    let f = setup();
    fund(&f, 100_000); // 70,000 retained, under the 100,000 floor
    assert_eq!(f.client.available_to_sweep(), 0);
    assert_eq!(
        f.client.try_sweep_to_treasury(&f.operator),
        Err(Ok(Error::NothingToSweep))
    );
}

#[test]
fn only_an_operator_sweeps() {
    let f = setup();
    fund(&f, 1_000_000);
    assert!(f.client.try_sweep_to_treasury(&f.outsider).is_err());
}

// ---------------------------------------------------------------- configuration

#[test]
fn the_floor_and_split_are_adjustable_by_the_admin_only() {
    let f = setup();
    assert!(f
        .client
        .try_set_config(
            &f.outsider,
            &Config { min_floor: 0, treasury_split_bps: 0 }
        )
        .is_err());

    f.client.set_config(
        &f.admin,
        &Config {
            min_floor: 0,
            treasury_split_bps: 0,
        },
    );
    fund(&f, 100_000);
    // Nothing to the treasury now, and no floor holding anything back.
    assert_eq!(f.token.balance(&f.treasury), 0);
    assert_eq!(f.client.available_for_loan(), 100_000);
}

#[test]
fn a_split_over_a_hundred_percent_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_set_config(
            &f.admin,
            &Config {
                min_floor: 0,
                treasury_split_bps: 10_001
            }
        ),
        Err(Ok(Error::InvalidBps))
    );
}

#[test]
fn the_full_cycle_leaves_the_books_balanced() {
    let f = setup_with(0, 0);
    fund(&f, 1_000_000);

    // Lend, absorb a loss, get repaid, sweep what is left.
    f.client.loan_to_pool(&f.operator, &f.pool, &f.manager, &400_000);
    f.client
        .cover_shortfall(&f.operator, &f.pool, &f.manager, &100_000);
    f.client.repay_loan(&f.manager, &f.pool, &400_000);

    let stats = f.client.stats_of();
    assert_eq!(stats.balance, 900_000); // 1,000,000 in, 100,000 absorbed
    assert_eq!(stats.total_loaned, 0);
    assert_eq!(stats.total_shortfall_covered, 100_000);

    assert_eq!(f.client.sweep_to_treasury(&f.operator), 900_000);
    assert_eq!(f.client.stats_of().balance, 0);
    assert_eq!(f.token.balance(&f.treasury), 900_000);
}
