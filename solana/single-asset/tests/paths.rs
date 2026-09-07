//! The paths a single-asset pool can take that are not the happy one.
//!
//! Mirrors the Soroban suite, because a binding that only proves the good case proves the
//! least interesting thing about itself: every rejection below is a place where money
//! would otherwise move when it should not.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// Core error codes travel verbatim through `ProgramError::Custom`, so a test can name the
/// protocol condition rather than an Anchor offset.
fn failed_with(result: Result<(), String>, code: u32) {
    let err = result.expect_err("expected a rejection");
    assert!(
        err.contains(&format!("Custom({code})")),
        "expected custom error {code}, got {err}"
    );
}

// ---------------------------------------------------------------- deposits

#[test]
fn a_deposit_past_the_target_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET + 1);
    failed_with(deposit(&mut svm, &p, &who, asset, shares, TARGET + 1), 203);
}

#[test]
fn a_deposit_below_the_minimum_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |t| t.min_investment = 10_000);
    let (who, asset, shares) = holder_with(&mut svm, &p, 10_000);
    failed_with(deposit(&mut svm, &p, &who, asset, shares, 9_999), 202);
}

#[test]
fn a_deposit_after_the_epoch_ends_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |t| t.epoch_end_time = 1_000);
    let (who, asset, shares) = holder_with(&mut svm, &p, 500_000);
    warp(&mut svm, 1_001);
    failed_with(deposit(&mut svm, &p, &who, asset, shares, 500_000), 201);
}

#[test]
fn a_deposit_into_a_closed_pool_is_rejected() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");

    let (other, other_asset, other_shares) = holder_with(&mut svm, &p, 500_000);
    failed_with(deposit(&mut svm, &p, &other, other_asset, other_shares, 500_000), 200);
}

// ---------------------------------------------------------------- authority

#[test]
fn only_the_authority_can_close_the_epoch() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let stranger = funded(&mut svm);
    assert!(
        close_epoch(&mut svm, &p, &stranger, true).is_err(),
        "a stranger closed the epoch"
    );
    close_epoch(&mut svm, &p, &p.authority, true).expect("the authority may");
}

#[test]
fn only_the_spv_can_draw_funds() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");

    let impostor = funded(&mut svm);
    let impostor_asset = create_token_account(&mut svm, &p.authority, &p.asset, &impostor.pubkey());
    let draw = piron_single_asset_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: impostor_asset,
        spv: impostor.pubkey(), token_program: spl_token::ID,
    };
    assert!(
        send(&mut svm, &impostor, &[], ix(p.program, draw.to_account_metas(None),
            piron_single_asset_solana::instruction::WithdrawForInvestment { amount: TARGET }.data())).is_err(),
        "someone other than the SPV drew the raise"
    );
}

// ---------------------------------------------------------------- underfunded

#[test]
fn an_underfunded_epoch_goes_to_emergency_and_refunds_at_par() {
    let mut svm = LiteSVM::new();
    // 80% threshold on a 1,000,000,000 target: 700,000,000 is short.
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, 700_000_000);
    deposit(&mut svm, &p, &who, asset, shares, 700_000_000).expect("partial raise");

    warp(&mut svm, 1_001);
    close_epoch(&mut svm, &p, &p.authority, false).expect("close");

    // Nothing was invested, so a refund pays one unit per share and the vault empties.
    let before = token_balance(&svm, &asset);
    withdraw(&mut svm, &p, &who, asset, shares, 700_000_000).expect("refund");
    assert_eq!(token_balance(&svm, &asset) - before, 700_000_000, "par, no fee");
    assert_eq!(token_balance(&svm, &p.vault), 0);
    assert_eq!(token_balance(&svm, &shares), 0);
}

// ---------------------------------------------------------------- redemption

#[test]
fn an_invested_pool_cannot_be_redeemed_before_it_settles() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");

    let auth = piron_single_asset_solana::accounts::Authority { pool: p.pool, authority: p.authority.pubkey() };
    send(&mut svm, &p.authority, &[], ix(p.program, auth.to_account_metas(None),
        piron_single_asset_solana::instruction::ConfirmInvestment {
            actual_amount: TARGET, proof: "p".to_string(),
        }.data())).expect("confirm");

    // The maturity date passing is not settlement: the money is still with the SPV.
    warp(&mut svm, 200_000);
    failed_with(withdraw(&mut svm, &p, &who, asset, shares, 1), 204);
}

#[test]
fn a_position_cannot_be_redeemed_twice() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");

    let auth = piron_single_asset_solana::accounts::Authority { pool: p.pool, authority: p.authority.pubkey() };
    send(&mut svm, &p.authority, &[], ix(p.program, auth.to_account_metas(None),
        piron_single_asset_solana::instruction::ConfirmInvestment {
            actual_amount: TARGET, proof: "p".to_string(),
        }.data())).expect("confirm");

    warp(&mut svm, 200_000);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, TARGET);
    let settle = piron_single_asset_solana::accounts::SpvPaysIn {
        pool: p.pool, vault: p.vault, share_mint: p.share_mint, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, settle.to_account_metas(None),
        piron_single_asset_solana::instruction::SettleMaturity { final_amount: TARGET }.data()))
        .expect("settle");

    withdraw(&mut svm, &p, &who, asset, shares, 1).expect("first redemption");
    // The shares are gone, so there is nothing left to claim against.
    assert!(
        withdraw(&mut svm, &p, &who, asset, shares, 1).is_err(),
        "a burnt position was redeemed again"
    );
}

// ---------------------------------------------------------------- emergency

/// Sets up a funded, closed pool and returns the declare-emergency call.
fn funded_and_closed(svm: &mut LiteSVM) -> Pool {
    let p = open_pool(svm, |_| {});
    let (who, asset, shares) = holder_with(svm, &p, TARGET);
    deposit(svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(svm, &p, &p.authority, true).expect("close");
    p
}

fn declare_emergency(svm: &mut LiteSVM, p: &Pool) -> Result<(), String> {
    let auth = piron_single_asset_solana::accounts::Authority {
        pool: p.pool, authority: p.authority.pubkey(),
    };
    send(svm, &p.authority, &[], ix(p.program, auth.to_account_metas(None),
        piron_single_asset_solana::instruction::DeclareEmergency { trigger: "spv".to_string() }.data()))
}

#[test]
fn an_emergency_is_allowed_while_the_vault_is_whole() {
    let mut svm = LiteSVM::new();
    let p = funded_and_closed(&mut svm);
    declare_emergency(&mut svm, &p).expect("nothing has left the vault");
}

#[test]
fn an_emergency_is_refused_once_the_spv_holds_the_money() {
    let mut svm = LiteSVM::new();
    let p = funded_and_closed(&mut svm);

    let draw = piron_single_asset_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, draw.to_account_metas(None),
        piron_single_asset_solana::instruction::WithdrawForInvestment { amount: TARGET }.data()))
        .expect("draw");

    // Par is now a promise the vault cannot keep: the first claimants would take their
    // full face and the rest would find nothing.
    failed_with(declare_emergency(&mut svm, &p), 229);
}

// ---------------------------------------------------------------- the coupon flow

/// A coupon-bearing pool, funded, closed and with the money out with the SPV.
fn invested_coupon_pool(svm: &mut LiteSVM) -> (Pool, Keypair, Pubkey, Pubkey) {
    let p = open_pool(svm, |t| {
        t.instrument_type = 1;
        t.coupon_count = 2;
        t.coupon_dates[0] = 40_000;
        t.coupon_dates[1] = 80_000;
        t.coupon_rates[0] = 300;
        t.coupon_rates[1] = 300;
    });
    let (who, asset, shares) = holder_with(svm, &p, TARGET);
    deposit(svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(svm, &p, &p.authority, TARGET).expect("confirm");
    (p, who, asset, shares)
}

#[test]
fn a_coupon_arrives_and_is_claimed_by_the_only_holder() {
    let mut svm = LiteSVM::new();
    let (p, who, asset, shares) = invested_coupon_pool(&mut svm);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 30_000_000);
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute");
    claim_coupon(&mut svm, &p, &who, asset, shares).expect("claim");

    // One holder, so the whole coupon is theirs.
    assert_eq!(token_balance(&svm, &asset), 30_000_000);
    assert_eq!(load_pool(&svm, &p.pool).total_coupons_claimed, 30_000_000);
}

#[test]
fn a_coupon_is_split_by_shareholding() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |t| {
        t.instrument_type = 1;
        t.coupon_count = 1;
        t.coupon_dates[0] = 40_000;
        t.coupon_rates[0] = 300;
    });
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 750_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 250_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 750_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 250_000_000).expect("bob");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 40_000_000);
    record_coupon(&mut svm, &p, &p.spv, 40_000_000).expect("coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute");

    claim_coupon(&mut svm, &p, &alice, aa, a_s).expect("alice claims");
    claim_coupon(&mut svm, &p, &bob, ba, b_s).expect("bob claims");

    // Three quarters and one quarter, by shares held.
    assert_eq!(token_balance(&svm, &aa), 30_000_000);
    assert_eq!(token_balance(&svm, &ba), 10_000_000);
}

#[test]
fn a_coupon_cannot_be_claimed_before_it_is_distributed() {
    let mut svm = LiteSVM::new();
    let (p, who, asset, shares) = invested_coupon_pool(&mut svm);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 30_000_000);
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("coupon");
    // Received is not the same as distributed; claiming early would pay out of principal.
    assert!(claim_coupon(&mut svm, &p, &who, asset, shares).is_err(), "claimed undistributed");
}

#[test]
fn the_same_coupon_cannot_be_claimed_twice() {
    let mut svm = LiteSVM::new();
    let (p, who, asset, shares) = invested_coupon_pool(&mut svm);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 30_000_000);
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute");
    claim_coupon(&mut svm, &p, &who, asset, shares).expect("first claim");

    svm.expire_blockhash();
    assert!(
        claim_coupon(&mut svm, &p, &who, asset, shares).is_err(),
        "the same coupon was paid twice"
    );
}

#[test]
fn a_second_coupon_is_claimable_after_the_first() {
    let mut svm = LiteSVM::new();
    let (p, who, asset, shares) = invested_coupon_pool(&mut svm);

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 60_000_000);
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("first coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute");
    claim_coupon(&mut svm, &p, &who, asset, shares).expect("claim");

    svm.expire_blockhash();
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("second coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute again");
    claim_coupon(&mut svm, &p, &who, asset, shares).expect("claim again");
    assert_eq!(token_balance(&svm, &asset), 60_000_000);
}

#[test]
fn distributing_with_nothing_received_is_refused() {
    let mut svm = LiteSVM::new();
    let (p, _who, _a, _s) = invested_coupon_pool(&mut svm);
    assert!(
        distribute_coupons(&mut svm, &p, &p.authority).is_err(),
        "distributed coupons that never arrived"
    );
}

#[test]
fn only_the_spv_pays_a_coupon_in() {
    let mut svm = LiteSVM::new();
    let (p, _who, _a, _s) = invested_coupon_pool(&mut svm);
    let impostor = funded(&mut svm);
    let their_asset = create_token_account(&mut svm, &p.authority, &p.asset, &impostor.pubkey());
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &their_asset, 30_000_000);
    assert!(record_coupon(&mut svm, &p, &impostor, 30_000_000).is_err(), "not the SPV");
}

#[test]
fn only_the_authority_distributes() {
    let mut svm = LiteSVM::new();
    let (p, _who, _a, _s) = invested_coupon_pool(&mut svm);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 30_000_000);
    record_coupon(&mut svm, &p, &p.spv, 30_000_000).expect("coupon");
    assert!(distribute_coupons(&mut svm, &p, &p.spv).is_err(), "the SPV distributed");
}

#[test]
fn a_coupon_on_a_discount_instrument_is_refused() {
    let mut svm = LiteSVM::new();
    // The default fixture is a discount bond: no coupon schedule, so no coupon is due.
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 1_000_000);
    assert!(record_coupon(&mut svm, &p, &p.spv, 1_000_000).is_err(), "a discount bond paid a coupon");
}

// ---------------------------------------------------------------- settlement

#[test]
fn a_settlement_short_of_par_is_shared_across_holders() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 600_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 400_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 600_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 400_000_000).expect("bob");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    // The SPV recovers 60%. Nobody is made whole and nobody is favoured.
    warp(&mut svm, 100_001);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 600_000_000);
    settle_maturity(&mut svm, &p, &p.spv, 600_000_000).expect("settle");

    withdraw(&mut svm, &p, &alice, aa, a_s, 600_000_000).expect("alice out");
    withdraw(&mut svm, &p, &bob, ba, b_s, 400_000_000).expect("bob out");

    // 60% each, less the 0.5% withdrawal fee.
    assert_eq!(token_balance(&svm, &aa), 358_200_000);
    assert_eq!(token_balance(&svm, &ba), 238_800_000);
    assert_eq!(token_balance(&svm, &p.vault), 0, "the pool is empty");
}

#[test]
fn a_settlement_above_par_pays_the_gain_out() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    warp(&mut svm, 100_001);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 1_050_000_000);
    settle_maturity(&mut svm, &p, &p.spv, 1_050_000_000).expect("settle");
    withdraw(&mut svm, &p, &who, asset, shares, TARGET).expect("redeem");

    // 1,050,000,000 less the 0.5% fee.
    assert_eq!(token_balance(&svm, &asset), 1_044_750_000);
}

#[test]
fn a_settlement_can_only_happen_once() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    warp(&mut svm, 100_001);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 2_000_000_000);
    settle_maturity(&mut svm, &p, &p.spv, 1_000_000_000).expect("settle");
    svm.expire_blockhash();
    assert!(
        settle_maturity(&mut svm, &p, &p.spv, 1_000_000_000).is_err(),
        "settled twice, which would reprice everyone who had not yet redeemed"
    );
}

#[test]
fn a_holder_who_waits_gets_the_same_price_as_one_who_rushes() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    warp(&mut svm, 100_001);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 900_000_000);
    settle_maturity(&mut svm, &p, &p.spv, 900_000_000).expect("settle");

    // The divisor is frozen at settlement. Were it the live supply, Alice's redemption
    // would shrink it and Bob would be paid at a better price for having waited.
    withdraw(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice out");
    withdraw(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob out");
    assert_eq!(token_balance(&svm, &aa), token_balance(&svm, &ba));
    assert_eq!(token_balance(&svm, &aa), 447_750_000);
}
