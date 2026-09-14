use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use super::*;

struct Fixture {
    env: Env,
    escrow: PoolEscrowClient<'static>,
    token: TokenClient<'static>,
    manager: Address,
    investor: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(issuer);
    let token = TokenClient::new(&env, &asset.address());
    let minter = StellarAssetClient::new(&env, &asset.address());

    let manager = Address::generate(&env);
    let investor = Address::generate(&env);
    minter.mint(&investor, &1_000_000);

    let deployer = Address::generate(&env);
    let id = env.register(PoolEscrow, (deployer, asset.address()));
    let escrow = PoolEscrowClient::new(&env, &id);
    escrow.bind_manager(&manager);

    Fixture {
        env,
        escrow,
        token,
        manager,
        investor,
    }
}

#[test]
fn a_manager_can_only_be_bound_once() {
    let f = setup();
    let usurper = Address::generate(&f.env);
    assert_eq!(
        f.escrow.try_bind_manager(&usurper),
        Err(Ok(Error::AlreadyBound))
    );
}

#[test]
fn an_escrow_starts_empty_and_bound_to_its_manager() {
    let f = setup();
    assert_eq!(f.escrow.balance(), 0);
    assert_eq!(f.escrow.manager(), f.manager);
    assert_eq!(f.escrow.total_in(), 0);
}

#[test]
fn taking_funds_moves_them_into_custody() {
    let f = setup();
    f.escrow.take(&f.investor, &400_000);

    assert_eq!(f.escrow.balance(), 400_000);
    assert_eq!(f.escrow.total_in(), 400_000);
    assert_eq!(f.token.balance(&f.investor), 600_000);
}

#[test]
fn releasing_funds_pays_the_receiver() {
    let f = setup();
    f.escrow.take(&f.investor, &400_000);

    let spv = Address::generate(&f.env);
    f.escrow.release(&spv, &400_000);

    assert_eq!(f.escrow.balance(), 0);
    assert_eq!(f.escrow.total_out(), 400_000);
    assert_eq!(f.token.balance(&spv), 400_000);
}

#[test]
fn releasing_more_than_is_held_is_refused() {
    let f = setup();
    f.escrow.take(&f.investor, &400_000);
    let spv = Address::generate(&f.env);
    assert_eq!(
        f.escrow.try_release(&spv, &400_001),
        Err(Ok(Error::InsufficientBalance))
    );
}

#[test]
fn zero_and_negative_amounts_are_refused() {
    let f = setup();
    assert_eq!(
        f.escrow.try_take(&f.investor, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        f.escrow.try_release(&f.investor, &-1),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
#[should_panic]
fn nobody_but_the_manager_can_release() {
    // No mocked auth: the manager's signature is missing, so the call cannot proceed.
    let env = Env::default();
    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract_v2(issuer);
    let manager = Address::generate(&env);
    let thief = Address::generate(&env);

    let deployer = Address::generate(&env);
    let id = env.register(PoolEscrow, (deployer.clone(), asset.address()));
    let escrow = PoolEscrowClient::new(&env, &id);
    env.mock_all_auths();
    escrow.bind_manager(&manager);
    env.set_auths(&[]);
    escrow.release(&thief, &1);
}

/// H4. A donation straight to the escrow address does not move the pool's books.
/// Before the fix `balance()` returned the raw token balance, so anyone could reprice a
/// pool by transferring tokens to it — the ERC-4626 inflation setup.
#[test]
fn a_donation_does_not_move_the_pools_books() {
    let f = setup();
    f.escrow.take(&f.investor, &1_000);
    assert_eq!(f.escrow.balance(), 1_000);

    // Somebody transfers straight to the address, bypassing `take`.
    f.token.transfer(&f.investor, &f.escrow.address, &500);
    assert_eq!(f.escrow.balance(), 1_000, "a donation moved the books");
    assert_eq!(f.escrow.untracked(), 500);

    // It becomes the pool's only when the manager says so.
    assert_eq!(f.escrow.sync_untracked(), 500);
    assert_eq!(f.escrow.balance(), 1_500);
    assert_eq!(f.escrow.untracked(), 0);
}
