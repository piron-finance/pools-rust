//! Contract tests for the Locked pool manager.
//!
//! The interest maths is already proved in `piron-core`. What is tested here is custody,
//! ownership, tier administration, and that a position survives across calls.

use pool_escrow::{PoolEscrow, PoolEscrowClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use super::*;

const PRINCIPAL: i128 = 1_000_000;
const START: u64 = 1_000;
const TERM: u64 = 180 * 86_400;
/// Interest on 1,000,000 at 12% for 180 days.
const INTEREST: i128 = 59_178;

struct Fixture {
    env: Env,
    client: LockedPoolManagerClient<'static>,
    escrow: PoolEscrowClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    investor: Address,
    treasury: Address,
}

fn tier() -> StoredTier {
    StoredTier {
        duration_days: 180,
        apy_bps: 1_200,
        early_exit_penalty_bps: 500,
        min_deposit: 1_000,
        is_active: true,
    }
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let investor = Address::generate(&env);
    let sponsor = Address::generate(&env);
    let deployer = Address::generate(&env);

    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(issuer);
    let token = TokenClient::new(&env, &asset.address());
    let minter = StellarAssetClient::new(&env, &asset.address());
    minter.mint(&investor, &2_000_000);
    minter.mint(&sponsor, &1_000_000);

    let escrow_id = env.register(PoolEscrow, (deployer, asset.address()));
    let escrow = PoolEscrowClient::new(&env, &escrow_id);

    let roles = Roles {
        admin: admin.clone(),
        operator: admin.clone(),
        spv: admin.clone(),
        emergency: admin.clone(),
        treasury: treasury.clone(),
    };
    let id = env.register(
        LockedPoolManager,
        (roles, escrow_id.clone(), asset.address(), 1_000i128),
    );
    escrow.bind_manager(&id);

    let client = LockedPoolManagerClient::new(&env, &id);
    client.add_tier(&admin, &tier());

    // Yield has to come from somewhere. In production it is SPV returns; here a sponsor
    // pre-funds the escrow so redemptions can actually pay interest.
    escrow.take(&sponsor, &500_000);

    Fixture {
        env,
        client,
        escrow,
        token,
        admin,
        investor,
        treasury,
    }
}

// ---------------------------------------------------------------- tiers

#[test]
fn a_pool_opens_with_the_tiers_its_admin_adds() {
    let f = setup();
    assert_eq!(f.client.tier_count(), 1);
    assert_eq!(f.client.tier(&0).apy_bps, 1_200);
}

#[test]
fn only_the_admin_can_add_a_tier() {
    let f = setup();
    assert_eq!(
        f.client.try_add_tier(&f.investor, &tier()),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn a_tier_over_the_rate_ceiling_is_refused() {
    let f = setup();
    let mut absurd = tier();
    absurd.apy_bps = 9_000;
    assert_eq!(
        f.client.try_add_tier(&f.admin, &absurd),
        Err(Ok(Error::TierApyTooHigh))
    );
}

#[test]
fn a_closed_tier_takes_no_new_deposits() {
    let f = setup();
    f.client.set_tier_active(&f.admin, &0, &false);
    assert_eq!(
        f.client.try_deposit(&f.investor, &PRINCIPAL, &0, &1),
        Err(Ok(Error::TierInactive))
    );
}

// ---------------------------------------------------------------- deposits

#[test]
fn an_at_maturity_deposit_locks_the_whole_principal() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);

    let p = f.client.position(&id);
    assert_eq!(p.principal_deposited, PRINCIPAL);
    assert_eq!(p.full_interest_amount, INTEREST);
    assert_eq!(p.expected_maturity_payout, PRINCIPAL + INTEREST);
    assert_eq!(p.lock_end, START + TERM);
    assert!(!p.interest_paid);

    // Every cent is in custody.
    assert_eq!(f.escrow.balance(), 500_000 + PRINCIPAL);
    assert_eq!(f.token.balance(&f.investor), 1_000_000);
}

#[test]
fn an_upfront_deposit_pays_its_interest_on_the_day() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &0);

    let p = f.client.position(&id);
    assert!(p.interest_paid);
    assert_eq!(p.invested_amount, PRINCIPAL - INTEREST);
    assert_eq!(p.expected_maturity_payout, PRINCIPAL);

    // The investor is down principal but up the interest immediately.
    assert_eq!(f.token.balance(&f.investor), 1_000_000 + INTEREST);
    assert_eq!(f.escrow.balance(), 500_000 + PRINCIPAL - INTEREST);
}

#[test]
fn positions_are_numbered_and_independent() {
    let f = setup();
    let first = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    let second = f.client.deposit(&f.investor, &500_000, &0, &1);

    assert_ne!(first, second);
    assert_eq!(f.client.position(&first).principal_deposited, PRINCIPAL);
    assert_eq!(f.client.position(&second).principal_deposited, 500_000);
    assert_eq!(f.client.pool_metrics().total_positions, 2);
    assert_eq!(f.client.pool_metrics().active_positions, 2);
}

#[test]
fn a_deposit_below_the_tier_minimum_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_deposit(&f.investor, &999, &0, &1),
        Err(Ok(Error::BelowMinInvestment))
    );
}

#[test]
fn a_deposit_into_a_tier_that_does_not_exist_is_refused() {
    let f = setup();
    assert_eq!(
        f.client.try_deposit(&f.investor, &PRINCIPAL, &7, &1),
        Err(Ok(Error::InvalidTier))
    );
}

#[test]
fn a_closed_pool_takes_no_deposits_but_honours_open_positions() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.client.set_active(&f.admin, &false);

    assert_eq!(
        f.client.try_deposit(&f.investor, &PRINCIPAL, &0, &1),
        Err(Ok(Error::PoolClosed))
    );

    f.env.ledger().set_timestamp(START + TERM);
    assert_eq!(f.client.redeem(&f.investor, &id), PRINCIPAL + INTEREST);
}

// ---------------------------------------------------------------- redemption

#[test]
fn redeeming_at_maturity_pays_principal_and_interest() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM);

    assert_eq!(f.client.redeem(&f.investor, &id), PRINCIPAL + INTEREST);
    assert_eq!(f.token.balance(&f.investor), 2_000_000 + INTEREST);
    assert_eq!(f.escrow.balance(), 500_000 - INTEREST);
    assert_eq!(f.client.pool_metrics().active_positions, 0);
}

#[test]
fn redeeming_before_maturity_is_refused() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM - 1);
    assert_eq!(
        f.client.try_redeem(&f.investor, &id),
        Err(Ok(Error::PositionNotMatured))
    );
}

#[test]
fn a_position_can_only_be_redeemed_once() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM);
    f.client.redeem(&f.investor, &id);
    assert_eq!(
        f.client.try_redeem(&f.investor, &id),
        Err(Ok(Error::PositionAlreadyRedeemed))
    );
}

#[test]
fn nobody_can_redeem_someone_elses_position() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    let stranger = Address::generate(&f.env);
    f.env.ledger().set_timestamp(START + TERM);
    assert_eq!(
        f.client.try_redeem(&stranger, &id),
        Err(Ok(Error::NotPositionOwner))
    );
}

// ---------------------------------------------------------------- early exit

#[test]
fn leaving_early_pays_the_penalty_to_the_treasury() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM / 2);

    // Quoted before committing.
    assert_eq!(f.client.quote_early_exit(&id), 978_110);

    assert_eq!(f.client.exit_early(&f.investor, &id), 978_110);
    assert_eq!(f.token.balance(&f.treasury), 51_479);
    assert_eq!(f.token.balance(&f.investor), 1_000_000 + 978_110);
    assert_eq!(f.client.pool_metrics().active_positions, 0);
}

#[test]
fn leaving_after_maturity_is_a_redemption_not_an_exit() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM);
    assert_eq!(
        f.client.try_exit_early(&f.investor, &id),
        Err(Ok(Error::PositionAlreadyMatured))
    );
}

#[test]
fn an_exited_position_cannot_be_redeemed() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM / 2);
    f.client.exit_early(&f.investor, &id);

    f.env.ledger().set_timestamp(START + TERM);
    assert_eq!(
        f.client.try_redeem(&f.investor, &id),
        Err(Ok(Error::PositionAlreadyExited))
    );
}

// ---------------------------------------------------------------- rollover

#[test]
fn rolling_over_compounds_without_the_money_leaving() {
    let f = setup();
    let first = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    f.env.ledger().set_timestamp(START + TERM);

    let balance_before = f.token.balance(&f.investor);
    let second = f.client.roll_over(&f.investor, &first, &0, &1);

    // Nothing was paid out; the matured value became the new principal.
    assert_eq!(f.token.balance(&f.investor), balance_before);
    let rolled = f.client.position(&second);
    assert_eq!(rolled.principal_deposited, PRINCIPAL + INTEREST);
    assert_eq!(rolled.full_interest_amount, 62_680);
    assert_eq!(rolled.lock_start, START + TERM);

    // The old position is closed, not redeemable.
    assert_eq!(
        f.client.try_redeem(&f.investor, &first),
        Err(Ok(Error::PositionNotRedeemable))
    );
}

#[test]
fn an_immature_position_cannot_be_rolled() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    assert_eq!(
        f.client.try_roll_over(&f.investor, &id, &0, &1),
        Err(Ok(Error::PositionNotMatured))
    );
}

// ---------------------------------------------------------------- views & storage

#[test]
fn a_position_reports_its_accrual_and_countdown() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);

    assert_eq!(f.client.days_remaining(&id), 180);
    assert_eq!(f.client.position_value(&id), 0);

    f.env.ledger().set_timestamp(START + TERM / 2);
    assert_eq!(f.client.days_remaining(&id), 90);
    assert_eq!(f.client.position_value(&id), 29_589);
}

#[test]
fn a_position_survives_past_a_default_entry_lifetime() {
    let f = setup();
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += ttl::PERSISTENT_THRESHOLD_LEDGERS + 1;
    });

    assert_eq!(f.client.position(&id).principal_deposited, PRINCIPAL);
}

#[test]
fn an_unknown_position_is_reported_not_invented() {
    let f = setup();
    // Compared loosely because StoredPosition carries no Debug in the contract build.
    assert!(f.client.try_position(&999).is_err());
}

// ---------------------------------------------------------------- spv allocation

#[test]
fn only_uncommitted_cash_can_go_to_the_spv() {
    let f = setup();
    // 500,000 sponsor float plus a 1,000,000 deposit owing 1,059,178 at maturity.
    let id = f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    let _ = id;

    // Held 1,500,000, committed 1,059,178 — the rest is free.
    assert_eq!(f.client.deployable(), 1_500_000 - (PRINCIPAL + INTEREST));

    let free = f.client.deployable();
    assert_eq!(
        f.client.try_deploy_to_spv(&f.admin, &(free + 1)),
        Err(Ok(Error::InsufficientLiquidity))
    );
    f.client.deploy_to_spv(&f.admin, &free);
    assert_eq!(f.client.deployable(), 0);
}

#[test]
fn returning_capital_frees_it_again() {
    let f = setup();
    f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    let free = f.client.deployable();
    f.client.deploy_to_spv(&f.admin, &free);

    f.client.return_from_spv(&f.admin, &free);
    assert_eq!(f.client.deployable(), free);
    assert_eq!(f.client.pool_metrics().total_invested, 0);
}

#[test]
fn a_committed_pool_will_not_send_out_what_it_owes() {
    let f = setup();
    // Every position's payout stays covered, whatever the SPV asks for.
    f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    let free = f.client.deployable();
    f.client.deploy_to_spv(&f.admin, &free);

    f.env.ledger().set_timestamp(START + TERM);
    // The redemption still pays in full, because the pool never lent out its cover.
    assert_eq!(f.client.redeem(&f.investor, &1), PRINCIPAL + INTEREST);
}

#[test]
fn only_the_spv_role_moves_capital() {
    let f = setup();
    f.client.deposit(&f.investor, &PRINCIPAL, &0, &1);
    assert_eq!(
        f.client.try_deploy_to_spv(&f.investor, &1_000),
        Err(Ok(Error::Unauthorized))
    );
}
