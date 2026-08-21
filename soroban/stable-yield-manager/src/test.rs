//! Contract tests for the Stable Yield manager.
//!
//! NAV arithmetic is proved in `piron-core`. What is tested here is that share pricing,
//! custody, the holding period and the batched queue hold together across calls.

use pool_escrow::{PoolEscrow, PoolEscrowClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use super::*;

const START: u64 = 1_000;
const DAY: u64 = 86_400;
const YEAR: u64 = 365 * DAY;
const ONE: i128 = 1_000_000_000_000_000_000; // 1e18

struct Fixture {
    env: Env,
    client: StableYieldManagerClient<'static>,
    escrow: PoolEscrowClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    spv: Address,
    treasury: Address,
    alice: Address,
    bob: Address,
}

fn config(reserve_ratio_bps: u32) -> Config {
    Config {
        min_investment: 1_000,
        holding_period: 7 * DAY,
        withdrawal_fee_bps: 50,
        min_absolute_reserve: 0,
        reserve_ratio_bps,
        is_active: true,
    }
}

fn setup_with(cfg: Config) -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let admin = Address::generate(&env);
    let spv = Address::generate(&env);
    let treasury = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let deployer = Address::generate(&env);

    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(issuer);
    let token = TokenClient::new(&env, &asset.address());
    let minter = StellarAssetClient::new(&env, &asset.address());
    minter.mint(&alice, &2_000_000);
    minter.mint(&bob, &2_000_000);
    minter.mint(&spv, &2_000_000);

    let escrow_id = env.register(PoolEscrow, (deployer, asset.address()));
    let escrow = PoolEscrowClient::new(&env, &escrow_id);

    let roles = Roles {
        admin: admin.clone(),
        operator: admin.clone(),
        spv: spv.clone(),
        emergency: admin.clone(),
        treasury: treasury.clone(),
    };
    let id = env.register(
        StableYieldManager,
        (roles, escrow_id.clone(), asset.address(), cfg),
    );
    escrow.bind_manager(&id);

    let client = StableYieldManagerClient::new(&env, &id);

    Fixture {
        env,
        client,
        escrow,
        token,
        admin,
        spv,
        treasury,
        alice,
        bob,
    }
}

fn setup() -> Fixture {
    setup_with(config(0))
}

fn discounted(purchase: i128, face: i128, now: u64) -> StoredInstrument {
    StoredInstrument {
        instrument_type: 0,
        purchase_price: purchase,
        face_value: face,
        purchase_date: now,
        maturity_date: now + YEAR,
        annual_coupon_rate: 0,
        coupon_frequency: 0,
        next_coupon_due_date: 0,
        coupons_paid: 0,
        is_active: true,
    }
}

// ---------------------------------------------------------------- pricing

#[test]
fn an_empty_pool_prices_at_one() {
    let f = setup();
    assert_eq!(f.client.total_nav(), 0);
    assert_eq!(f.client.nav_per_share(), ONE);
}

#[test]
fn the_first_deposit_buys_shares_one_for_one() {
    let f = setup();
    let shares = f.client.deposit(&f.alice, &1_000_000);

    assert_eq!(shares, 1_000_000);
    assert_eq!(f.client.total_supply(), 1_000_000);
    assert_eq!(f.escrow.balance(), 1_000_000);
    assert_eq!(f.client.nav_per_share(), ONE);
    assert_eq!(f.client.holding_value(&f.alice), 1_000_000);
}

#[test]
fn nav_counts_instruments_as_well_as_cash() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);

    // Half the pool goes into a discounted bill.
    f.client.deploy_capital(&f.spv, &500_000);
    assert_eq!(f.escrow.balance(), 500_000);
    f.client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));

    // Cash plus the bill at cost: unchanged.
    assert_eq!(f.client.total_nav(), 1_000_000);
    assert_eq!(f.client.nav_per_share(), ONE);
}

#[test]
fn the_share_price_rises_as_instruments_accrete() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    f.client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));

    f.env.ledger().set_timestamp(START + YEAR);
    // The bill matured at face: 550,000 + 500,000 cash.
    assert_eq!(f.client.total_nav(), 1_050_000);
    assert_eq!(f.client.nav_per_share(), ONE * 105 / 100);
    assert_eq!(f.client.holding_value(&f.alice), 1_050_000);
}

#[test]
fn a_later_depositor_buys_in_at_the_higher_price() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    f.client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));
    f.env.ledger().set_timestamp(START + YEAR);

    // 105,000 at 1.05 buys 100,000 shares, so Alice's gain is not diluted.
    assert_eq!(f.client.deposit(&f.bob, &105_000), 100_000);
    assert_eq!(f.client.holding_value(&f.alice), 1_050_000);
    assert_eq!(f.client.holding_value(&f.bob), 105_000);
}

#[test]
fn a_deposit_below_the_minimum_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_deposit(&f.alice, &999),
        Err(Ok(Error::BelowMinInvestment))
    );
}

#[test]
fn a_closed_pool_takes_no_deposits() {
    let f = setup_with(Config {
        is_active: false,
        ..config(0)
    });
    assert_eq!(
        f.client.try_deposit(&f.alice, &1_000_000),
        Err(Ok(Error::PoolInactive))
    );
}

// ---------------------------------------------------------------- the queue

#[test]
fn a_holder_must_sit_out_the_holding_period_before_queueing() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);

    assert_eq!(
        f.client.try_request_withdrawal(&f.alice, &100_000),
        Err(Ok(Error::HoldingPeriodNotMet))
    );

    f.env.ledger().set_timestamp(START + 7 * DAY);
    assert_eq!(f.client.request_withdrawal(&f.alice, &100_000), 0);
    assert_eq!(f.client.queue_length(), 1);
}

#[test]
fn queueing_more_shares_than_held_is_refused() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.env.ledger().set_timestamp(START + 7 * DAY);
    assert_eq!(
        f.client.try_request_withdrawal(&f.alice, &1_000_001),
        Err(Ok(Error::InsufficientShares))
    );
}

#[test]
fn processing_the_queue_pays_the_holder_and_the_treasury() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.env.ledger().set_timestamp(START + 7 * DAY);
    f.client.request_withdrawal(&f.alice, &100_000);

    assert_eq!(f.client.process_withdrawals(&f.admin, &10), 1);

    // 100,000 shares at par, less the 50bp fee.
    assert_eq!(f.token.balance(&f.alice), 1_000_000 + 99_500);
    assert_eq!(f.token.balance(&f.treasury), 500);
    assert_eq!(f.client.shares_of(&f.alice), 900_000);
    assert_eq!(f.client.total_supply(), 900_000);
    assert_eq!(f.client.queue_length(), 0);
}

#[test]
fn withdrawals_are_served_in_order() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deposit(&f.bob, &1_000_000);
    f.env.ledger().set_timestamp(START + 7 * DAY);

    f.client.request_withdrawal(&f.alice, &100_000);
    f.client.request_withdrawal(&f.bob, &100_000);

    // One at a time, head first.
    assert_eq!(f.client.process_withdrawals(&f.admin, &1), 1);
    assert_eq!(f.client.shares_of(&f.alice), 900_000);
    assert_eq!(f.client.shares_of(&f.bob), 1_000_000);

    assert_eq!(f.client.process_withdrawals(&f.admin, &1), 1);
    assert_eq!(f.client.shares_of(&f.bob), 900_000);
}

#[test]
fn a_queue_with_nothing_in_it_has_nothing_to_do() {
    let f = setup();
    assert_eq!(
        f.client.try_process_withdrawals(&f.admin, &10),
        Err(Ok(Error::QueueEmpty))
    );
}

#[test]
fn the_queue_stalls_rather_than_overpaying_when_capital_is_deployed() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    // Almost everything goes out to the SPV.
    f.client.deploy_capital(&f.spv, &950_000);
    f.client
        .add_instrument(&f.admin, &discounted(950_000, 950_000, START));

    f.env.ledger().set_timestamp(START + 7 * DAY);
    f.client.request_withdrawal(&f.alice, &100_000);

    // 100,000 is owed but only 50,000 is liquid; nothing is paid.
    assert_eq!(f.client.process_withdrawals(&f.admin, &10), 0);
    assert_eq!(f.client.shares_of(&f.alice), 1_000_000);
    assert_eq!(f.client.queue_length(), 1);

    // Once the SPV returns capital the same request clears.
    f.client.return_capital(&f.spv, &500_000);
    assert_eq!(f.client.process_withdrawals(&f.admin, &10), 1);
    assert_eq!(f.client.shares_of(&f.alice), 900_000);
}

#[test]
fn a_batch_larger_than_the_footprint_allows_is_clamped_not_refused() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.env.ledger().set_timestamp(START + 7 * DAY);
    f.client.request_withdrawal(&f.alice, &10_000);

    // Asking for more than MAX_WITHDRAWAL_BATCH settles what it can rather than erroring.
    assert_eq!(f.client.process_withdrawals(&f.admin, &1_000), 1);
}

// ---------------------------------------------------------------- reserves

#[test]
fn the_reserve_holds_back_capital_from_deployment() {
    // 20% must stay liquid.
    let f = setup_with(config(2_000));
    f.client.deposit(&f.alice, &1_000_000);

    assert_eq!(f.client.deployable(), 800_000);
    assert_eq!(
        f.client.try_deploy_capital(&f.spv, &800_001),
        Err(Ok(Error::ReserveBreached))
    );
    f.client.deploy_capital(&f.spv, &800_000);
    assert_eq!(f.escrow.balance(), 200_000);
    assert_eq!(f.client.deployable(), 0);
}

// ---------------------------------------------------------------- authorisation

#[test]
fn only_the_spv_moves_capital() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    assert_eq!(
        f.client.try_deploy_capital(&f.alice, &100_000),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        f.client.try_return_capital(&f.alice, &100_000),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn only_an_operator_records_instruments_or_serves_the_queue() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    assert_eq!(
        f.client
            .try_add_instrument(&f.alice, &discounted(1, 1, START)),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        f.client.try_process_withdrawals(&f.alice, &10),
        Err(Ok(Error::Unauthorized))
    );
}

// ---------------------------------------------------------------- portfolio

#[test]
fn closing_an_instrument_takes_it_out_of_nav() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));

    f.env.ledger().set_timestamp(START + YEAR);
    assert_eq!(f.client.total_nav(), 1_050_000);

    // Sold or matured: the SPV returns the proceeds and the holding closes.
    f.client.close_instrument(&f.admin, &index);
    f.client.return_capital(&f.spv, &550_000);
    assert_eq!(f.client.total_nav(), 1_050_000);
    assert_eq!(f.client.instrument_count(), 1);
}

#[test]
fn an_instrument_that_does_not_exist_cannot_be_closed() {
    let f = setup();
    assert_eq!(
        f.client.try_close_instrument(&f.admin, &7),
        Err(Ok(Error::InstrumentNotFound))
    );
}
