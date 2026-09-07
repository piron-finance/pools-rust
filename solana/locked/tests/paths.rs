//! The paths a locked pool can take that are not the happy one.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_locked_solana::state::StoredTier;
use solana_signer::Signer;

fn failed_with(result: Result<(), String>, code: u32) {
    let err = result.expect_err("expected a rejection");
    assert!(err.contains(&format!("Custom({code})")), "expected {code}, got {err}");
}

// ---------------------------------------------------------------- tiers

#[test]
fn only_the_authority_can_add_a_tier() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    let stranger = funded(&mut svm);
    assert!(add_tier(&mut svm, &p, &stranger, standard_tier()).is_err(), "a stranger added a tier");
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("the authority may");
}

#[test]
fn a_tier_with_no_duration_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    let mut t = standard_tier();
    t.duration_days = 0;
    // A zero-day lock is not a lock: it would mature the instant it opened.
    assert!(add_tier(&mut svm, &p, &p.authority, t).is_err(), "a zero-day tier was accepted");
}

#[test]
fn a_deposit_into_a_tier_that_does_not_exist_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    assert!(deposit(&mut svm, &p, &who, acc, PRINCIPAL, 7, 1, 1).is_err(), "tier 7 does not exist");
}

#[test]
fn a_closed_tier_takes_no_new_deposits() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    let mut t = standard_tier();
    t.is_active = false;
    add_tier(&mut svm, &p, &p.authority, t).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    assert!(deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).is_err(), "a closed tier took a deposit");
}

#[test]
fn a_deposit_below_the_tier_minimum_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    let mut t = standard_tier();
    t.min_deposit = 10_000_000;
    add_tier(&mut svm, &p, &p.authority, t).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 10_000_000);
    assert!(deposit(&mut svm, &p, &who, acc, 9_999_999, 0, 1, 1).is_err(), "below the tier minimum");
}

// ---------------------------------------------------------------- positions

#[test]
fn positions_are_numbered_and_independent() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");

    let (a, a_acc) = holder_with(&mut svm, &p, PRINCIPAL);
    let (b, b_acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &a, a_acc, PRINCIPAL, 0, 1, 1).expect("first");
    deposit(&mut svm, &p, &b, b_acc, PRINCIPAL, 0, 1, 2).expect("second");

    let pool = load_pool(&svm, &p.pool);
    assert_eq!(pool.total_positions, 2);
    assert_eq!(pool.active_positions, 2);
    assert_eq!(pool.next_position_id, 3, "ids advance and never rewind");
}

#[test]
fn redeeming_before_maturity_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");
    failed_with(redeem(&mut svm, &p, &who, acc, 1), 308);
}

#[test]
fn nobody_can_redeem_someone_elses_position() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (owner, owner_acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &owner, owner_acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    let (thief, thief_acc) = holder_with(&mut svm, &p, 0);
    warp_days(&mut svm, 2);
    assert!(redeem(&mut svm, &p, &thief, thief_acc, 1).is_err(), "a stranger redeemed a position");
}

#[test]
fn a_position_can_only_be_redeemed_once() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    // The SPV returns principal plus the interest owed, so the vault can pay.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 100_000_000);
    let ret = piron_locked_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, ret.to_account_metas(None),
        piron_locked_solana::instruction::ReturnFromSpv { amount: 100_000_000 }.data())).expect("return");

    warp_days(&mut svm, 2);
    redeem(&mut svm, &p, &who, acc, 1).expect("first redemption");
    assert!(redeem(&mut svm, &p, &who, acc, 1).is_err(), "redeemed twice");
}

#[test]
fn an_exited_position_cannot_then_be_redeemed() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 100_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    // Half way through the term: still an early exit, not a matured one.
    warp_seconds(&mut svm, 43_200);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");
    warp_days(&mut svm, 2);
    assert!(redeem(&mut svm, &p, &who, acc, 1).is_err(), "an exited position was redeemed");
}

// ---------------------------------------------------------------- capital

#[test]
fn only_the_spv_moves_capital() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    let impostor = funded(&mut svm);
    let impostor_acc = create_token_account(&mut svm, &p.authority, &p.asset, &impostor.pubkey());
    let draw = piron_locked_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: impostor_acc,
        spv: impostor.pubkey(), token_program: spl_token::ID,
    };
    assert!(
        send(&mut svm, &impostor, &[], ix(p.program, draw.to_account_metas(None),
            piron_locked_solana::instruction::DeployToSpv { amount: PRINCIPAL }.data())).is_err(),
        "a stranger drew the principal"
    );
}

#[test]
fn an_early_exit_without_a_funded_reserve_is_refused_rather_than_paid_short() {
    let mut svm = LiteSVM::new();
    // No reserve funding: the vault holds only principal, so the accrued interest has
    // nowhere to come from.
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    warp_seconds(&mut svm, 43_200);
    let before = token_balance(&svm, &acc);
    assert!(exit_early(&mut svm, &p, &who, acc, 1).is_err(), "paid an exit it could not fund");
    assert_eq!(token_balance(&svm, &acc), before, "and nothing moved");
    assert_eq!(token_balance(&svm, &p.vault), PRINCIPAL, "the vault is untouched");
}

#[test]
fn leaving_after_maturity_is_a_redemption_not_an_exit() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 100_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, PRINCIPAL);
    deposit(&mut svm, &p, &who, acc, PRINCIPAL, 0, 1, 1).expect("deposit");

    // Past the lock end there is no early exit to take: the penalty would be charged on a
    // term the holder actually served.
    warp_days(&mut svm, 2);
    failed_with(exit_early(&mut svm, &p, &who, acc, 1), 310);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 100_000_000);
    let ret = piron_locked_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, ret.to_account_metas(None),
        piron_locked_solana::instruction::ReturnFromSpv { amount: 100_000_000 }.data())).expect("return");
    redeem(&mut svm, &p, &who, acc, 1).expect("redemption is the way out");
}
