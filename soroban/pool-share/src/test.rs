//! Contract tests for the share token.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{Address, Env, String};

use super::*;

struct Fixture {
    env: Env,
    client: PoolShareClient<'static>,
    manager: Address,
    alice: Address,
    bob: Address,
    outsider: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let manager = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let outsider = Address::generate(&env);

    let id = env.register(
        PoolShare,
        (
            manager.clone(),
            Metadata {
                name: String::from_str(&env, "Piron 90d T-Bill"),
                symbol: String::from_str(&env, "pTBILL"),
                decimals: 7u32,
            },
        ),
    );

    Fixture {
        client: PoolShareClient::new(&env, &id),
        env,
        manager,
        alice,
        bob,
        outsider,
    }
}

#[test]
fn a_new_token_carries_its_pools_identity() {
    let f = setup();
    assert_eq!(f.client.symbol(), String::from_str(&f.env, "pTBILL"));
    assert_eq!(f.client.decimals(), 7);
    assert_eq!(f.client.total_supply(), 0);
    assert_eq!(f.client.manager(), f.manager);
}

#[test]
fn only_the_manager_issues_or_retires_shares() {
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    assert_eq!(f.client.balance(&f.alice), 1_000_000);
    assert_eq!(f.client.total_supply(), 1_000_000);

    // Anyone else minting would be inventing claims on the pool's assets.
    let env = Env::default();
    let manager = Address::generate(&env);
    let id = env.register(
        PoolShare,
        (
            manager,
            Metadata {
                name: String::from_str(&env, "p"),
                symbol: String::from_str(&env, "p"),
                decimals: 7u32,
            },
        ),
    );
    let client = PoolShareClient::new(&env, &id);
    // No mocked auth: the manager's signature is missing.
    assert!(client.try_mint(&Address::generate(&env), &1).is_err());
}

#[test]
fn burning_reduces_the_holder_and_the_supply_together() {
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    f.client.burn(&f.alice, &400_000);
    assert_eq!(f.client.balance(&f.alice), 600_000);
    assert_eq!(f.client.total_supply(), 600_000);
}

#[test]
fn nobody_can_be_burnt_below_zero() {
    let f = setup();
    f.client.mint(&f.alice, &1_000);
    assert_eq!(
        f.client.try_burn(&f.alice, &1_001),
        Err(Ok(Error::InsufficientBalance))
    );
}

#[test]
fn a_position_can_be_sent_to_someone_else() {
    // The whole point of a share token: a holding is portable.
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    f.client.transfer(&f.alice, &f.bob, &250_000);

    assert_eq!(f.client.balance(&f.alice), 750_000);
    assert_eq!(f.client.balance(&f.bob), 250_000);
    // Transfers move claims around; they never change how many exist.
    assert_eq!(f.client.total_supply(), 1_000_000);
}

#[test]
fn a_transfer_beyond_the_balance_is_refused() {
    let f = setup();
    f.client.mint(&f.alice, &1_000);
    assert_eq!(
        f.client.try_transfer(&f.alice, &f.bob, &1_001),
        Err(Ok(Error::InsufficientBalance))
    );
}

#[test]
fn a_custodian_can_move_shares_it_has_been_approved_for() {
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    f.client.approve(&f.alice, &f.bob, &300_000, &0);
    assert_eq!(f.client.allowance(&f.alice, &f.bob), 300_000);

    f.client.transfer_from(&f.bob, &f.alice, &f.outsider, &200_000);
    assert_eq!(f.client.balance(&f.outsider), 200_000);
    assert_eq!(f.client.allowance(&f.alice, &f.bob), 100_000);
}

#[test]
fn spending_past_an_allowance_is_refused() {
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    f.client.approve(&f.alice, &f.bob, &100_000, &0);
    assert_eq!(
        f.client.try_transfer_from(&f.bob, &f.alice, &f.outsider, &100_001),
        Err(Ok(Error::InsufficientAllowance))
    );
}

#[test]
fn an_allowance_can_be_withdrawn() {
    let f = setup();
    f.client.mint(&f.alice, &1_000_000);
    f.client.approve(&f.alice, &f.bob, &100_000, &0);
    f.client.approve(&f.alice, &f.bob, &0, &0);
    assert!(f
        .client
        .try_transfer_from(&f.bob, &f.alice, &f.outsider, &1)
        .is_err());
}

#[test]
fn zero_and_negative_amounts_are_refused_everywhere() {
    let f = setup();
    f.client.mint(&f.alice, &1_000);
    assert_eq!(f.client.try_mint(&f.alice, &0), Err(Ok(Error::InvalidAmount)));
    assert_eq!(f.client.try_burn(&f.alice, &0), Err(Ok(Error::InvalidAmount)));
    assert_eq!(
        f.client.try_transfer(&f.alice, &f.bob, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        f.client.try_approve(&f.alice, &f.bob, &-1, &0),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn an_emptied_balance_leaves_nothing_behind() {
    let f = setup();
    f.client.mint(&f.alice, &1_000);
    f.client.transfer(&f.alice, &f.bob, &1_000);
    assert_eq!(f.client.balance(&f.alice), 0);
    assert_eq!(f.client.balance(&f.bob), 1_000);
}

/// M4. The SEP-41 expiry is honoured. Before the fix it was accepted and discarded, so
/// every approval was permanent behind a signature that granted a temporary one.
#[test]
fn an_approval_stops_working_at_the_ledger_it_names() {
    let f = setup();
    f.client.mint(&f.alice, &1_000);

    let expires = f.env.ledger().sequence() + 10;
    f.client.approve(&f.alice, &f.bob, &500, &expires);
    assert_eq!(f.client.allowance(&f.alice, &f.bob), 500);

    f.env.ledger().set_sequence_number(expires + 1);
    assert_eq!(f.client.allowance(&f.alice, &f.bob), 0, "the approval outlived its ledger");
    assert_eq!(
        f.client.try_transfer_from(&f.bob, &f.alice, &f.bob, &100),
        Err(Ok(Error::InsufficientAllowance))
    );
}
