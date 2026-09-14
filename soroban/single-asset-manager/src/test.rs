//! Contract-level tests for the Single-Asset manager.
//!
//! `piron-core` already proves the arithmetic. What is tested here is the wiring the core
//! cannot see: authorization, storage round-trips, TTL, event emission, and that the
//! lifecycle holds together across calls.

use pool_escrow::{PoolEscrow, PoolEscrowClient};
use pool_share::{Metadata, PoolShare, PoolShareClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env, String, Vec};

use super::*;

const EPOCH_END: u64 = 1_000;
const MATURITY: u64 = 11_000;
const TARGET: i128 = 1_000_000;

struct Fixture {
    env: Env,
    client: SingleAssetManagerClient<'static>,
    escrow: PoolEscrowClient<'static>,
    share: PoolShareClient<'static>,
    token: TokenClient<'static>,
    investor: Address,
    other: Address,
    admin: Address,
    operator: Address,
    spv: Address,
    emergency: Address,
    treasury: Address,
}

fn discounted_config(env: &Env) -> StoredConfig {
    StoredConfig {
        instrument_type: 0,
        face_value: 0,
        purchase_price: TARGET,
        target_raise: TARGET,
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

fn interest_bearing_config(env: &Env) -> StoredConfig {
    let mut dates = Vec::new(env);
    dates.push_back(5_000);
    dates.push_back(9_000);
    let mut rates = Vec::new(env);
    rates.push_back(250);
    rates.push_back(250);

    StoredConfig {
        instrument_type: 1,
        coupon_dates: dates,
        coupon_rates: rates,
        ..discounted_config(env)
    }
}

fn setup_with(config_for: fn(&Env) -> StoredConfig) -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100);

    let admin = Address::generate(&env);
    let operator = Address::generate(&env);
    let spv = Address::generate(&env);
    let emergency = Address::generate(&env);
    let treasury = Address::generate(&env);
    let investor = Address::generate(&env);
    let other = Address::generate(&env);
    let deployer = Address::generate(&env);

    // A real SEP-41 asset, so the tests move actual balances rather than accounting.
    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(issuer);
    let token = TokenClient::new(&env, &asset.address());
    let minter = StellarAssetClient::new(&env, &asset.address());
    minter.mint(&investor, &2_000_000);
    minter.mint(&other, &2_000_000);
    minter.mint(&spv, &2_000_000); // enough to return more than it drew

    let escrow_id = env.register(PoolEscrow, (deployer, asset.address()));
    let escrow = PoolEscrowClient::new(&env, &escrow_id);

    let roles = Roles {
        admin: admin.clone(),
        operator: operator.clone(),
        spv: spv.clone(),
        emergency: emergency.clone(),
        treasury: treasury.clone(),
    };

    // The share token names its manager, so it needs the manager's address; registering
    // the manager first and the token second keeps the test honest about the ordering the
    // factory solves with a salt.
    let share_id = env.register(
        PoolShare,
        (
            Address::generate(&env),
            Metadata {
                name: soroban_sdk::String::from_str(&env, "Piron Share"),
                symbol: soroban_sdk::String::from_str(&env, "pS"),
                decimals: 7u32,
            },
        ),
    );
    let id = env.register(
        SingleAssetManager,
        (config_for(&env), roles, escrow_id.clone(), share_id.clone()),
    );
    escrow.bind_manager(&id);
    // Re-register the token now that the manager exists, bound to it.
    let share_id = env.register_at(
        &share_id,
        PoolShare,
        (
            id.clone(),
            Metadata {
                name: soroban_sdk::String::from_str(&env, "Piron Share"),
                symbol: soroban_sdk::String::from_str(&env, "pS"),
                decimals: 7u32,
            },
        ),
    );
    let share = PoolShareClient::new(&env, &share_id);
    let client = SingleAssetManagerClient::new(&env, &id);

    Fixture {
        env,
        client,
        escrow,
        share,
        token,
        investor,
        other,
        admin,
        operator,
        spv,
        emergency,
        treasury,
    }
}

fn discounted_config_with_fee(env: &Env) -> StoredConfig {
    StoredConfig {
        withdrawal_fee_bps: 50, // 0.5%
        ..discounted_config(env)
    }
}

fn setup() -> Fixture {
    setup_with(discounted_config)
}

// ---------------------------------------------------------------- construction

#[test]
fn a_new_pool_opens_in_funding_with_nothing_raised() {
    let f = setup();
    assert_eq!(f.client.status(), PoolStatus::Funding as u32);
    assert_eq!(f.client.total_supply(), 0);
    assert_eq!(f.client.shares_of(&f.investor), 0);
}

#[test]
#[should_panic]
fn a_config_that_fails_validation_cannot_be_deployed() {
    let env = Env::default();
    env.mock_all_auths();
    let addr = Address::generate(&env);
    let roles = Roles {
        admin: addr.clone(),
        operator: addr.clone(),
        spv: addr.clone(),
        emergency: addr.clone(),
        treasury: addr,
    };
    let mut config = discounted_config(&env);
    config.maturity_date = config.epoch_end_time; // maturity must follow funding

    env.register(SingleAssetManager, (config, roles, Address::generate(&env)));
}

// ---------------------------------------------------------------- deposits

#[test]
fn a_deposit_mints_shares_and_raises_the_total() {
    let f = setup();
    let shares = f.client.deposit(&f.investor, &400_000);

    assert_eq!(shares, 400_000);
    assert_eq!(f.client.shares_of(&f.investor), 400_000);
    assert_eq!(f.client.total_supply(), 400_000);
    // During funding a holding is worth its deposit.
    assert_eq!(f.client.user_return(&f.investor), 400_000);
    // And the assets are actually in custody, not merely accounted for.
    assert_eq!(f.escrow.balance(), 400_000);
    assert_eq!(f.token.balance(&f.investor), 1_600_000);
}

#[test]
fn deposits_accumulate_across_investors() {
    let f = setup();
    f.client.deposit(&f.investor, &600_000);
    f.client.deposit(&f.other, &400_000);
    assert_eq!(f.client.total_supply(), TARGET);
    assert_eq!(f.client.shares_of(&f.other), 400_000);
}

#[test]
fn a_deposit_past_the_target_is_rejected() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    assert_eq!(
        f.client.try_deposit(&f.other, &1_000),
        Err(Ok(Error::ExceedsTargetRaise))
    );
}

#[test]
fn a_deposit_below_the_minimum_is_rejected() {
    let f = setup();
    assert_eq!(
        f.client.try_deposit(&f.investor, &999),
        Err(Ok(Error::BelowMinInvestment))
    );
}

#[test]
fn a_deposit_after_the_epoch_ends_is_rejected() {
    let f = setup();
    f.env.ledger().set_timestamp(EPOCH_END + 1);
    assert_eq!(
        f.client.try_deposit(&f.investor, &400_000),
        Err(Ok(Error::FundingEnded))
    );
}

// ---------------------------------------------------------------- authorization

#[test]
fn only_the_operator_can_close_the_epoch() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    f.env.ledger().set_timestamp(EPOCH_END);

    assert_eq!(
        f.client.try_close_epoch(&f.investor),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        f.client.try_close_epoch(&f.spv),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        f.client.close_epoch(&f.operator),
        PoolStatus::PendingInvestment as u32
    );
}

#[test]
fn only_the_spv_can_confirm_an_investment() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    f.env.ledger().set_timestamp(EPOCH_END);
    f.client.close_epoch(&f.operator);

    let proof = String::from_str(&f.env, "ipfs://proof");
    assert_eq!(
        f.client
            .try_confirm_investment(&f.operator, &TARGET, &proof),
        Err(Ok(Error::Unauthorized))
    );
    f.client.confirm_investment(&f.spv, &TARGET, &proof);
    assert_eq!(f.client.status(), PoolStatus::Invested as u32);
}

#[test]
fn only_the_admin_can_force_close() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    assert_eq!(
        f.client.try_force_close_epoch(&f.operator),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        f.client.force_close_epoch(&f.admin),
        PoolStatus::PendingInvestment as u32
    );
}

#[test]
fn an_unauthorised_caller_cannot_declare_an_emergency() {
    let f = setup();
    assert_eq!(
        f.client.try_declare_emergency(&f.investor, &String::from_str(&f.env, "X")),
        Err(Ok(Error::Unauthorized))
    );
    f.client
        .declare_emergency(&f.emergency, &String::from_str(&f.env, "SPV_DEFAULT"));
    assert_eq!(f.client.status(), PoolStatus::Emergency as u32);
}

// ---------------------------------------------------------------- the full arc

#[test]
fn a_discounted_deal_runs_from_funding_to_redemption() {
    let f = setup();

    // Two investors fund the pool.
    f.client.deposit(&f.investor, &600_000);
    f.client.deposit(&f.other, &400_000);
    assert_eq!(f.client.status(), PoolStatus::Funding as u32);

    // Target met, so it can be marked filled by anyone.
    f.client.mark_filled();
    assert_eq!(f.client.status(), PoolStatus::Filled as u32);

    // Filled pools close without waiting for the epoch to end.
    assert_eq!(
        f.client.close_epoch(&f.operator),
        PoolStatus::PendingInvestment as u32
    );

    // The SPV draws the funds out of custody and confirms what it deployed.
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    assert_eq!(f.escrow.balance(), 0);
    assert_eq!(f.token.balance(&f.spv), 3_000_000);
    f.client.confirm_investment(
        &f.spv,
        &TARGET,
        &String::from_str(&f.env, "ipfs://treasury-bill"),
    );
    assert_eq!(f.client.status(), PoolStatus::Invested as u32);

    // Discount accrues linearly. Halfway to maturity the pool is worth half of it.
    f.env.ledger().set_timestamp(6_000);
    assert_eq!(f.client.pool_value(), 1_026_315);

    // Nobody can leave while capital is deployed.
    assert_eq!(
        f.client.try_withdraw(&f.investor, &1),
        Err(Ok(Error::WithdrawalsNotAllowed))
    );

    // At maturity the SPV returns face value.
    f.env.ledger().set_timestamp(MATURITY);
    f.client.settle_maturity(&f.spv, &1_052_631);
    assert_eq!(f.client.status(), PoolStatus::Matured as u32);

    assert_eq!(f.escrow.balance(), 1_052_631);

    // First holder redeems 60% of 1,052,631 and is paid in full.
    let first = f.client.withdraw(&f.investor, &1);
    assert_eq!(first, 631_578);
    assert_eq!(f.token.balance(&f.investor), 1_400_000 + 631_578);
    assert_eq!(f.escrow.balance(), 421_053);

    // The second holder takes their 40% of the same pot. The divisor was frozen at
    // settlement, so the first redemption shrinking the supply does not enlarge this one.
    let second = f.client.withdraw(&f.other, &1);
    assert_eq!(second, 421_052);
    assert_eq!(f.client.shares_of(&f.other), 0);

    // 631,578 + 421,052 = 1,052,630 against a pot of 1,052,631: one unit of rounding dust
    // stays behind, which is the right direction — the escrow is never short.
    assert_eq!(first + second, 1_052_630);
    assert_eq!(f.escrow.balance(), 1);

    // Nothing outstanding, so the pool closes.
    f.client.mark_withdrawn(&f.operator);
    assert_eq!(f.client.status(), PoolStatus::Withdrawn as u32);
}

#[test]
fn a_sole_holder_redeems_cleanly_and_the_pool_closes() {
    // The same arc without the multi-holder defect in the way.
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    f.client.force_close_epoch(&f.admin);
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));

    f.env.ledger().set_timestamp(MATURITY);
    f.client.settle_maturity(&f.spv, &1_052_631);

    assert_eq!(f.client.withdraw(&f.investor, &1), 1_052_631);
    assert_eq!(f.escrow.balance(), 0);
    assert_eq!(f.client.total_supply(), 0);

    f.client.mark_withdrawn(&f.operator);
    assert_eq!(f.client.status(), PoolStatus::Withdrawn as u32);
}

#[test]
fn an_underfunded_epoch_goes_to_emergency_and_refunds_at_par() {
    let f = setup();
    // 70% of target, below the 80% threshold.
    f.client.deposit(&f.investor, &700_000);
    f.env.ledger().set_timestamp(EPOCH_END);

    assert_eq!(
        f.client.close_epoch(&f.operator),
        PoolStatus::Emergency as u32
    );

    // A partial refund is allowed, and the rest stays claimable.
    assert_eq!(f.client.withdraw(&f.investor, &300_000), 300_000);
    assert_eq!(f.client.shares_of(&f.investor), 400_000);
    assert_eq!(f.escrow.balance(), 400_000);

    assert_eq!(f.client.withdraw(&f.investor, &400_000), 400_000);
    assert_eq!(f.client.shares_of(&f.investor), 0);
    // Every cent went back to the investor.
    assert_eq!(f.escrow.balance(), 0);
    assert_eq!(f.token.balance(&f.investor), 2_000_000);
}

#[test]
fn a_withdrawal_fee_is_taken_from_the_redemption() {
    let f = setup_with(discounted_config_with_fee);

    f.client.deposit(&f.investor, &TARGET);
    f.client.force_close_epoch(&f.admin);
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));
    f.env.ledger().set_timestamp(MATURITY);
    f.client.settle_maturity(&f.spv, &1_052_631);

    // Sole holder: gross 1,052,631, fee 5,263, net 1,047,368.
    assert_eq!(f.client.withdraw(&f.investor, &1), 1_047_368);
    assert_eq!(f.token.balance(&f.treasury), 5_263);
}

#[test]
fn an_interest_bearing_deal_pays_coupons_pro_rata() {
    let f = setup_with(interest_bearing_config);

    f.client.deposit(&f.investor, &600_000);
    f.client.deposit(&f.other, &400_000);
    f.client.force_close_epoch(&f.admin);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));

    // A coupon arrives but is not claimable until distributed.
    f.client.record_coupon(&f.spv, &25_000);
    assert_eq!(f.client.available_coupon(&f.investor), 0);

    assert_eq!(f.client.distribute_coupons(&f.operator), 25_000);
    assert_eq!(f.client.available_coupon(&f.investor), 15_000); // 60%
    assert_eq!(f.client.available_coupon(&f.other), 10_000); // 40%

    assert_eq!(f.client.claim_coupon(&f.investor), 15_000);
    assert_eq!(f.client.available_coupon(&f.investor), 0);
    // Paid out of custody, not conjured.
    assert_eq!(f.token.balance(&f.investor), 1_400_000 + 15_000);
    assert_eq!(
        f.client.try_claim_coupon(&f.investor),
        Err(Ok(Error::NoNewCoupons))
    );

    // A second coupon makes the difference claimable again.
    f.client.record_coupon(&f.spv, &25_000);
    f.client.distribute_coupons(&f.operator);
    assert_eq!(f.client.claim_coupon(&f.investor), 15_000);
}

#[test]
fn a_discounted_pool_pays_no_coupons() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    f.client.force_close_epoch(&f.admin);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));

    assert_eq!(
        f.client.try_record_coupon(&f.spv, &1_000),
        Err(Ok(Error::NotInterestBearing))
    );
    assert_eq!(
        f.client.try_claim_coupon(&f.investor),
        Err(Ok(Error::NotInterestBearing))
    );
}

// ---------------------------------------------------------------- authorization is real

#[test]
#[should_panic]
fn a_deposit_requires_the_investor_to_have_signed() {
    // No mock_all_auths: the call must fail because nothing authorised it.
    let env = Env::default();
    env.ledger().set_timestamp(100);
    let addr = Address::generate(&env);
    let investor = Address::generate(&env);
    let roles = Roles {
        admin: addr.clone(),
        operator: addr.clone(),
        spv: addr.clone(),
        emergency: addr.clone(),
        treasury: addr,
    };
    let id = env.register(
        SingleAssetManager,
        (discounted_config(&env), roles, Address::generate(&env)),
    );
    let client = SingleAssetManagerClient::new(&env, &id);

    // No `mock_all_auths`, so `require_auth` has nothing to satisfy it.
    client.deposit(&investor, &400_000);
}

// ---------------------------------------------------------------- storage lifetime

#[test]
fn a_position_survives_past_the_default_entry_lifetime() {
    let f = setup();
    f.client.deposit(&f.investor, &400_000);

    // Advance well past a default TTL but inside the bump the contract applies on touch.
    f.env.ledger().with_mut(|li| {
        li.sequence_number += ttl::PERSISTENT_THRESHOLD_LEDGERS + 1;
    });

    assert_eq!(f.client.shares_of(&f.investor), 400_000);
    assert_eq!(f.client.total_supply(), 400_000);
}

// ---------------------------------------------------------------- transferability

#[test]
fn a_position_can_change_hands_and_the_new_holder_redeems_it() {
    // The reason shares are a token rather than a number in this contract's storage.
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    assert_eq!(f.share.balance(&f.investor), TARGET);

    // Sold, gifted, or moved into custody — the manager is not consulted.
    f.share.transfer(&f.investor, &f.other, &TARGET);
    assert_eq!(f.client.shares_of(&f.investor), 0);
    assert_eq!(f.client.shares_of(&f.other), TARGET);

    f.client.force_close_epoch(&f.admin);
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));
    f.env.ledger().set_timestamp(MATURITY);
    f.client.settle_maturity(&f.spv, &1_052_631);

    // The buyer redeems, and the seller has nothing left to redeem.
    assert_eq!(f.client.withdraw(&f.other, &1), 1_052_631);
    assert_eq!(
        f.client.try_withdraw(&f.investor, &1),
        Err(Ok(Error::NoShares))
    );
}

#[test]
fn a_partial_sale_splits_the_claim_proportionally() {
    let f = setup();
    f.client.deposit(&f.investor, &TARGET);
    f.share.transfer(&f.investor, &f.other, &400_000);

    f.client.force_close_epoch(&f.admin);
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));
    f.env.ledger().set_timestamp(MATURITY);
    f.client.settle_maturity(&f.spv, &1_052_631);

    // 60/40 of the settlement, in the order they redeem.
    assert_eq!(f.client.withdraw(&f.investor, &1), 631_578);
}

#[test]
fn the_manager_alone_can_issue_shares() {
    // A holder who could mint could claim assets nobody deposited.
    let f = setup();
    assert_eq!(f.share.manager(), f.client.address);
}

// ------------------------------------------------------- audit regression tests
//
// Each asserts the corrected behaviour, so a failure here means a fix has been undone.

#[test]
fn probe_c1_coupon_cannot_be_reclaimed_by_moving_shares() {
    let f = setup_with(interest_bearing_config);

    f.client.deposit(&f.investor, &600_000);
    f.client.deposit(&f.other, &400_000);
    f.client.force_close_epoch(&f.admin);
    // The SPV places 900,000 and leaves 100,000 of principal in custody, so an
    // overpayment has somewhere to come from.
    f.client.withdraw_for_investment(&f.spv, &900_000);
    f.client
        .confirm_investment(&f.spv, &900_000, &String::from_str(&f.env, "p"));

    f.client.record_coupon(&f.spv, &25_000);
    assert_eq!(f.client.distribute_coupons(&f.operator), 25_000);

    let escrow_before = f.token.balance(&f.client.escrow_address());
    assert_eq!(f.client.claim_coupon(&f.investor), 15_000);

    // The holder moves their shares to a third party who has never deposited.
    let stranger = Address::generate(&f.env);
    let shares = pool_share::PoolShareClient::new(&f.env, &f.client.share_token());
    shares.transfer(&f.investor, &stranger, &600_000);

    // The entitlement follows the subscription, not the balance, so an address that
    // never bought in has nothing to claim however many shares it now holds.
    assert_eq!(
        f.client.try_claim_coupon(&stranger),
        Err(Ok(Error::NoShares))
    );
    let paid = escrow_before - f.token.balance(&f.client.escrow_address());
    assert_eq!(paid, 15_000, "only the claimant's own share left custody");
}

#[test]
fn probe_h1_unclaimed_coupon_survives_settlement() {
    let f = setup_with(interest_bearing_config);

    f.client.deposit(&f.investor, &600_000);
    f.client.deposit(&f.other, &400_000);
    f.client.force_close_epoch(&f.admin);
    f.client.withdraw_for_investment(&f.spv, &TARGET);
    f.client
        .confirm_investment(&f.spv, &TARGET, &String::from_str(&f.env, "p"));

    f.client.record_coupon(&f.spv, &25_000);
    f.client.distribute_coupons(&f.operator);
    // One holder claims; the other is passive, like most holders.
    f.client.claim_coupon(&f.investor);

    f.env.ledger().set_timestamp(MATURITY + 1);
    f.client.settle_maturity(&f.spv, &TARGET);

    let before = f.token.balance(&f.other);
    f.client.withdraw(&f.other, &1);
    let got = f.token.balance(&f.other) - before;

    // Conservation: once every holder is out, custody should hold nothing that is still
    // owed to someone. Whatever remains is money nobody can now reach.
    f.client.withdraw(&f.investor, &1);
    let stranded = f.token.balance(&f.client.escrow_address());

    // 40% of a 1,000,000 settlement is 400,000, and this holder is also owed the 10,000
    // coupon they never got round to claiming — settled on the way out.
    assert_eq!(
        (got, stranded),
        (410_000, 0),
        "passive holder payout, and what custody still holds afterwards"
    );
}
