//! The paths a stable-yield pool can take that are not the happy one.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

fn failed_with(result: Result<(), String>, code: u32) {
    let err = result.expect_err("expected a rejection");
    assert!(err.contains(&format!("Custom({code})")), "expected {code}, got {err}");
}

// ---------------------------------------------------------------- deposits

#[test]
fn a_deposit_below_the_minimum_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |c| c.0 = 10_000_000);
    let (who, a, s) = holder_with(&mut svm, &p, 10_000_000);
    assert!(deposit(&mut svm, &p, &who, a, s, 9_999_999).is_err(), "below the minimum");
}

#[test]
fn the_first_deposit_is_struck_at_par() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    // Nothing outstanding means no price to read, so one share is one unit.
    assert_eq!(token_balance(&svm, &s), 1_000_000_000);
}

#[test]
fn a_later_deposit_buys_at_the_price_yield_has_moved_to() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, a_asset, a_shares) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &alice, a_asset, a_shares, 1_000_000_000).expect("alice");

    // The SPV draws, then returns more than it took. The surplus is yield, so NAV rises
    // while the share count does not.
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 200_000_000);
    return_capital(&mut svm, &p, &p.spv, p.spv_asset, 1_000_000_000).expect("return");

    let after_yield = load_pool(&svm, &p.pool);
    assert_eq!(after_yield.total_nav, 1_200_000_000, "a 20% gain");

    // Bob pays the new price, so his shares are fewer than his assets.
    let (bob, b_asset, b_shares) = holder_with(&mut svm, &p, 1_200_000_000);
    deposit(&mut svm, &p, &bob, b_asset, b_shares, 1_200_000_000).expect("bob");
    assert_eq!(
        token_balance(&svm, &b_shares),
        1_000_000_000,
        "1,200,000,000 at 1.2 buys the same claim alice has"
    );
}

// ---------------------------------------------------------------- capital

#[test]
fn only_the_spv_can_draw_capital() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    let impostor = funded(&mut svm);
    let impostor_asset = create_token_account(&mut svm, &p.authority, &p.asset, &impostor.pubkey());
    assert!(
        deploy_capital(&mut svm, &p, &impostor, impostor_asset, 100_000_000).is_err(),
        "a stranger drew capital"
    );
}

#[test]
fn a_draw_that_would_breach_the_reserve_is_refused() {
    let mut svm = LiteSVM::new();
    // 20% has to stay behind, so at most 800,000,000 of a 1,000,000,000 pool is drawable.
    let p = open_pool(&mut svm, |c| c.4 = 2_000);
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    assert!(
        deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_001).is_err(),
        "drew through the reserve floor"
    );
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("up to the floor is fine");
    assert_eq!(token_balance(&svm, &p.vault), 200_000_000, "the floor is intact");
}

#[test]
fn a_draw_moves_value_between_lines_without_creating_any() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");

    let before = load_pool(&svm, &p.pool).total_nav;
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");
    let after = load_pool(&svm, &p.pool);

    assert_eq!(after.total_nav, before, "cash with the SPV still belongs to the pool");
    assert_eq!(after.in_flight, 800_000_000);
    assert_eq!(token_balance(&svm, &p.vault) + after.in_flight, after.total_nav, "the lines sum to NAV");
}

#[test]
fn returning_more_than_was_drawn_is_yield_not_a_negative_balance() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, a, s) = holder_with(&mut svm, &p, 1_000_000_000);
    deposit(&mut svm, &p, &who, a, s, 1_000_000_000).expect("deposit");
    deploy_capital(&mut svm, &p, &p.spv, p.spv_asset, 800_000_000).expect("draw");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 40_000_000);
    return_capital(&mut svm, &p, &p.spv, p.spv_asset, 840_000_000).expect("return");

    let pool = load_pool(&svm, &p.pool);
    assert_eq!(pool.in_flight, 0, "in-flight capital is retired first");
    assert_eq!(pool.total_nav, 1_040_000_000, "the surplus lands as a gain");
    assert_eq!(token_balance(&svm, &p.vault), pool.total_nav, "and it is all in the vault");
}

#[test]
fn a_zero_return_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    failed_with(return_capital(&mut svm, &p, &p.spv, p.spv_asset, 0), 103);
}

// ---------------------------------------------------------------- the queue account

#[test]
fn the_queue_is_larger_than_a_cpi_could_have_allocated() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let full = piron_stable_yield_solana::state::WithdrawalQueue::LEN;

    assert!(full > 10_240, "otherwise the client-created account would be unnecessary");
    assert_eq!(svm.get_account(&p.queue).unwrap().data.len(), full, "created at full size");
    assert_eq!(
        load_pool(&svm, &p.pool).queue,
        p.queue,
        "the pool records the address, since it cannot be derived from a seed"
    );
}
