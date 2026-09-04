//! Contract tests for the Stable Yield manager.
//!
//! NAV arithmetic is proved in `piron-core`. What is tested here is that share pricing,
//! custody, the holding period and the batched queue hold together across calls.

use pool_escrow::{PoolEscrow, PoolEscrowClient};
use pool_share::{Metadata, PoolShare};
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
    // The token names its manager and the manager names its token. In production the
    // factory breaks that circle with a salt; here the token is re-registered at the same
    // address once the manager exists.
    fn meta(env: &Env) -> Metadata {
        Metadata {
            name: soroban_sdk::String::from_str(env, "Piron Stable Yield"),
            symbol: soroban_sdk::String::from_str(env, "pSY"),
            decimals: 7u32,
        }
    }
    let share_id = env.register(PoolShare, (Address::generate(&env), meta(&env)));
    let id = env.register(
        StableYieldManager,
        (roles, escrow_id.clone(), asset.address(), cfg, share_id.clone()),
    );
    escrow.bind_manager(&id);
    env.register_at(&share_id, PoolShare, (id.clone(), meta(&env)));

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
    // The holding leaves the book rather than lingering as a dead entry: the purchase
    // cap counts entries, and a pool that never closes would otherwise run out of slots.
    assert_eq!(f.client.instrument_count(), 0);
}

#[test]
fn an_instrument_that_does_not_exist_cannot_be_closed() {
    let f = setup();
    assert_eq!(
        f.client.try_close_instrument(&f.admin, &7),
        Err(Ok(Error::InstrumentNotFound))
    );
}

// ---------------------------------------------------------------- instrument lifecycle

#[test]
fn a_coupon_comes_in_and_the_instrument_clock_advances() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);

    let mut bearing = discounted(500_000, 500_000, START);
    bearing.instrument_type = 1;
    bearing.annual_coupon_rate = 800; // 8%
    bearing.coupon_frequency = 4;
    bearing.next_coupon_due_date = START + YEAR / 4;
    let index = f.client.add_instrument(&f.admin, &bearing);

    // A quarter in, the coupon is accrued into the instrument's value but not yet paid.
    f.env.ledger().set_timestamp(START + YEAR / 4);
    let before = f.client.total_nav();
    assert_eq!(before, 1_010_000);

    f.client.record_instrument_coupon(&f.spv, &index, &10_000);

    // NAV is unchanged, and that is the point: the coupon moved from accrued receivable
    // to cash. Nothing was created, so nothing should be repriced. What changed is where
    // the value sits — and the instrument now accrues the *next* coupon from zero.
    assert_eq!(f.escrow.balance(), 500_000 + 10_000);
    assert_eq!(f.client.total_nav(), before);

    // Half a period on, it has begun accruing again.
    f.env.ledger().set_timestamp(START + YEAR / 4 + YEAR / 8);
    assert_eq!(f.client.total_nav(), 1_015_000);
}

#[test]
fn maturing_an_instrument_brings_the_proceeds_back_and_closes_it() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));

    f.env.ledger().set_timestamp(START + YEAR);
    assert_eq!(f.client.total_nav(), 1_050_000);

    f.client.mature_instrument(&f.spv, &index, &550_000);

    // Held as cash now rather than as a holding; NAV is unchanged by the settlement.
    assert_eq!(f.escrow.balance(), 1_050_000);
    assert_eq!(f.client.total_nav(), 1_050_000);
}

#[test]
fn a_short_return_at_maturity_shows_up_as_a_nav_fall() {
    // The pool took a loss. Pretending otherwise would misprice every share.
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));

    f.env.ledger().set_timestamp(START + YEAR);
    f.client.mature_instrument(&f.spv, &index, &400_000);

    assert_eq!(f.client.total_nav(), 900_000);
    assert!(f.client.nav_per_share() < ONE);
}

#[test]
fn only_the_spv_settles_instruments() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &100_000);
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(100_000, 110_000, START));
    assert_eq!(
        f.client.try_mature_instrument(&f.alice, &index, &110_000),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn an_instrument_settles_once() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &500_000);
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(500_000, 550_000, START));
    f.env.ledger().set_timestamp(START + YEAR);
    f.client.mature_instrument(&f.spv, &index, &550_000);
    assert_eq!(
        f.client.try_mature_instrument(&f.spv, &index, &550_000),
        Err(Ok(Error::InstrumentNotFound))
    );
}

#[test]
fn an_instrument_cannot_be_recorded_larger_than_the_capital_drawn() {
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);
    f.client.deploy_capital(&f.spv, &100_000);

    // Recording 150,000 against 100,000 in flight would clamp the balance at zero and
    // leave NAV counting the holding in full — 50,000 the pool never had.
    assert_eq!(
        f.client
            .try_add_instrument(&f.admin, &discounted(150_000, 160_000, START)),
        Err(Ok(Error::ExceedsInFlight))
    );

    let nav_before = f.client.total_nav();
    let index = f
        .client
        .add_instrument(&f.admin, &discounted(100_000, 110_000, START));
    assert_eq!(
        f.client.total_nav(),
        nav_before,
        "recording a purchase moves capital between lines, it does not create any"
    );
    assert_eq!(f.client.in_flight(), 0);
    let _ = index;
}

#[test]
fn a_rolling_pool_can_keep_buying_after_its_holdings_settle() {
    // The purchase cap counts entries in the book, so a settled holding left in place
    // would still occupy a slot. A pool that never closes would reach the cap on its
    // 33rd purchase ever, with every earlier instrument long since matured.
    let f = setup();
    f.client.deposit(&f.alice, &1_000_000);

    // Well past the 32-entry cap. Each holding is bought and settled at par, so the
    // SPV's balance is not what is being tested here.
    for _ in 0..40 {
        f.client.deploy_capital(&f.spv, &100_000);
        let index = f
            .client
            .add_instrument(&f.admin, &discounted(100_000, 100_000, START));
        assert_eq!(f.client.instrument_count(), 1, "the book holds one at a time");
        f.env.ledger().set_timestamp(START + YEAR);
        f.client.mature_instrument(&f.spv, &index, &100_000);
        assert_eq!(f.client.instrument_count(), 0, "a settled holding leaves the book");
        f.env.ledger().set_timestamp(START);
    }
}

#[test]
fn retiring_a_holding_keeps_the_others_valued() {
    let f = setup();
    f.client.deposit(&f.alice, &1_500_000);
    f.client.deploy_capital(&f.spv, &300_000);

    let a = f.client.add_instrument(&f.admin, &discounted(100_000, 100_000, START));
    let _b = f.client.add_instrument(&f.admin, &discounted(100_000, 100_000, START));
    let _c = f.client.add_instrument(&f.admin, &discounted(100_000, 100_000, START));
    assert_eq!(f.client.instrument_count(), 3);

    let nav_before = f.client.total_nav();

    // Retiring the first swap-removes the last into its slot. NAV must not move: the
    // holding's marked value becomes escrow cash.
    f.env.ledger().set_timestamp(START + YEAR);
    f.client.mature_instrument(&f.spv, &a, &100_000);
    assert_eq!(f.client.instrument_count(), 2);

    // The two survivors are both still marked, at face now that they are past maturity.
    assert_eq!(f.client.total_nav(), nav_before);
}
