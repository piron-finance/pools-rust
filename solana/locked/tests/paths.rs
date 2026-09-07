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

// ---------------------------------------------------------------- rolling over

#[test]
fn a_rollover_compounds_the_whole_payout_into_the_new_term() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 1).expect("roll");

    // A day of 12% on 1,000,000 is 328; the new term runs on 1,000,328, not 1,000,000.
    let old = load_position(&svm, &p, 1);
    let new = load_position(&svm, &p, 2);
    assert_eq!(old.status, 4, "closed as rolled over, not redeemed");
    assert_eq!(new.principal_deposited, 1_000_328);
    assert_eq!(token_balance(&svm, &acc), 0, "no money left the pool");
}

#[test]
fn an_upfront_rollover_pays_its_interest_on_day_one() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 0).expect("roll upfront");

    // The new term's own interest is paid out now; the principal stays locked.
    assert_eq!(token_balance(&svm, &acc), 328);
    assert!(load_position(&svm, &p, 2).interest_paid);
}

#[test]
fn a_position_cannot_be_rolled_before_it_matures() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");
    assert!(
        roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 1).is_err(),
        "rolled a term that had not run"
    );
}

#[test]
fn a_rolled_position_cannot_also_be_redeemed() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 1).expect("roll");
    // The payout is inside the new position now; paying it out again would double it.
    assert!(redeem(&mut svm, &p, &who, acc, 1).is_err(), "paid twice");
}

#[test]
fn nobody_can_roll_someone_elses_position() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (alice, alice_acc) = holder_with(&mut svm, &p, 1_000_000);
    let (mallory, mallory_acc) = holder_with(&mut svm, &p, 0);
    deposit(&mut svm, &p, &alice, alice_acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    assert!(
        roll_over(&mut svm, &p, &mallory, mallory_acc, 1, 2, 0, 1).is_err(),
        "rolled a position they did not own"
    );
}

#[test]
fn a_rollover_into_a_tier_that_does_not_exist_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    assert!(roll_over(&mut svm, &p, &who, acc, 1, 2, 7, 1).is_err(), "rolled into nothing");
}

#[test]
fn a_rollover_keeps_the_pools_books_straight() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_days(&mut svm, 1);
    roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 1).expect("roll");

    // The old principal leaves the book as the new, larger one joins it — counting both
    // would tell the SPV the pool is holding twice what it has.
    let pool = load_pool(&svm, &p.pool);
    assert_eq!(pool.total_principal_locked, 1_000_328);
    assert_eq!(pool.total_positions, 2);
    assert_eq!(pool.active_positions, 1, "one closed as another opened");
}

// ---------------------------------------------------------------- spv and debt

#[test]
fn the_whole_vault_is_drawable_and_the_reserve_carries_what_lands_early() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    // Deliberate: holding back principal plus promised interest would leave a locked pool
    // nothing to invest, and the yield paying that interest has to come from somewhere.
    deploy_to_spv(&mut svm, &p, &p.spv, 1_000_000).expect("the whole vault goes out");
    assert_eq!(token_balance(&svm, &p.vault), 0);

    // Which is exactly why the reserve exists: an exit before the SPV returns anything is
    // still paid, and the pool carries the debt.
    warp_seconds(&mut svm, 43_200);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit is still honoured");
    assert!(load_pool(&svm, &p.pool).reserve_debt > 0);
}

#[test]
fn the_spv_cannot_draw_more_than_the_vault_holds() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    assert!(deploy_to_spv(&mut svm, &p, &p.spv, 1_000_001).is_err(), "drew what was not there");
    assert!(deploy_to_spv(&mut svm, &p, &p.spv, 0).is_err(), "a nil draw is not a draw");

    // What the SPV returns is spendable again, yield included.
    deploy_to_spv(&mut svm, &p, &p.spv, 1_000_000).expect("draw");
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 200_000);
    return_from_spv(&mut svm, &p, &p.spv, 1_200_000).expect("return with yield");
    assert_eq!(token_balance(&svm, &p.vault), 1_200_000);
    deploy_to_spv(&mut svm, &p, &p.spv, 1_200_000).expect("out again");
}

#[test]
fn a_reserve_loan_is_recorded_as_debt_and_can_be_repaid() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    // Half a day in, the vault holds principal only — the interest has to be borrowed.
    warp_seconds(&mut svm, 43_200);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");

    let debt = load_pool(&svm, &p.pool).reserve_debt;
    assert!(debt > 0, "the loan was not recorded");

    // The SPV repays it out of the yield it earned on the capital.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, debt);
    settle_debt(&mut svm, &p, &p.spv, debt).expect("repay");
    assert_eq!(load_pool(&svm, &p.pool).reserve_debt, 0);
}

#[test]
fn only_the_spv_settles_the_pools_debt() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");
    warp_seconds(&mut svm, 43_200);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");

    assert!(settle_debt(&mut svm, &p, &p.authority, 1).is_err(), "not the SPV");
}

// ---------------------------------------------------------------- opening and closing

#[test]
fn closing_a_tier_stops_new_business_without_touching_the_old() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 2_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    set_tier_active(&mut svm, &p, &p.authority, 0, false).expect("close");
    assert!(
        deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 2).is_err(),
        "a closed tier took new money"
    );

    // The running position keeps the terms it was struck on.
    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");
    warp_days(&mut svm, 1);
    redeem(&mut svm, &p, &who, acc, 1).expect("the old position still pays");
    assert_eq!(token_balance(&svm, &acc), 1_000_000 + 1_000_328);
}

#[test]
fn only_the_authority_opens_or_closes_a_tier() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    assert!(set_tier_active(&mut svm, &p, &p.spv, 0, false).is_err(), "the SPV closed a tier");
}

#[test]
fn closing_a_tier_that_does_not_exist_is_refused() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    assert!(set_tier_active(&mut svm, &p, &p.authority, 7, false).is_err());
}

#[test]
fn a_closed_pool_still_lets_people_out() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 2_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    set_open(&mut svm, &p, &p.authority, false).expect("close the pool");
    assert!(deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 2).is_err(), "took new money");

    // Closing must never trap money already in the pool.
    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");
    warp_days(&mut svm, 1);
    redeem(&mut svm, &p, &who, acc, 1).expect("redemption still works");
}

#[test]
fn a_closed_pool_takes_no_rollovers() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");
    warp_days(&mut svm, 1);
    set_open(&mut svm, &p, &p.authority, false).expect("close");
    // A rollover opens a fresh term, which is new business by another name.
    assert!(roll_over(&mut svm, &p, &who, acc, 1, 2, 0, 1).is_err(), "rolled into a closed pool");
    redeem(&mut svm, &p, &who, acc, 1).expect("but they can still take their money");
}

#[test]
fn a_reopened_pool_takes_deposits_again() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 2_000_000);
    set_open(&mut svm, &p, &p.authority, false).expect("close");
    assert!(deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).is_err());
    set_open(&mut svm, &p, &p.authority, true).expect("reopen");
    svm.expire_blockhash();
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("open again");
}

// ---------------------------------------------------------------- interest terms

#[test]
fn upfront_interest_is_paid_on_day_one_and_maturity_returns_principal() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 0, 1).expect("upfront");

    assert_eq!(token_balance(&svm, &acc), 328, "the interest arrives immediately");
    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");
    warp_days(&mut svm, 1);
    redeem(&mut svm, &p, &who, acc, 1).expect("redeem");
    // Principal only at maturity: the interest was taken up front.
    assert_eq!(token_balance(&svm, &acc), 328 + 1_000_000);
}

#[test]
fn interest_at_maturity_arrives_all_at_once() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("at maturity");

    assert_eq!(token_balance(&svm, &acc), 0, "nothing until the term is up");
    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");
    warp_days(&mut svm, 1);
    redeem(&mut svm, &p, &who, acc, 1).expect("redeem");
    assert_eq!(token_balance(&svm, &acc), 1_000_328);
}

#[test]
fn an_early_exit_penalty_goes_to_the_treasury_not_the_pool() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    warp_seconds(&mut svm, 43_200);
    let before = token_balance(&svm, &p.treasury_asset);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");

    // Half a day of 12% on 1,000,000 is 164, so the position is worth 1,000,164 and the
    // 5% penalty on that is 50,008 — it bites the principal, which is what makes leaving
    // early a real cost rather than a forfeited coupon.
    assert_eq!(token_balance(&svm, &p.treasury_asset) - before, 50_008);
    assert_eq!(token_balance(&svm, &acc), 1_000_164 - 50_008);
}

#[test]
fn positions_in_different_tiers_keep_their_own_terms() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 0);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier 0");
    let mut long = standard_tier();
    long.duration_days = 30;
    long.apy_bps = 2_000;
    add_tier(&mut svm, &p, &p.authority, long).expect("tier 1");

    let (who, acc) = holder_with(&mut svm, &p, 2_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("short");
    deposit(&mut svm, &p, &who, acc, 1_000_000, 1, 1, 2).expect("long");
    // The SPV returns the yield, so the vault can pay the promised maturity.
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 10_000_000);
    return_from_spv(&mut svm, &p, &p.spv, 10_000_000).expect("yield returns");

    warp_days(&mut svm, 1);
    redeem(&mut svm, &p, &who, acc, 1).expect("the short one matures");
    assert!(redeem(&mut svm, &p, &who, acc, 2).is_err(), "the long one has not");

    warp_days(&mut svm, 30);
    svm.expire_blockhash();
    redeem(&mut svm, &p, &who, acc, 2).expect("and then it does");
    // 30 days of 20% on 1,000,000 is 16,438.
    assert_eq!(token_balance(&svm, &acc), 1_000_328 + 1_016_438);
}
