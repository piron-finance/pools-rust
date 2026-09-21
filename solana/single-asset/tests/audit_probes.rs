//! Regression tests for the audit findings. Each asserts the corrected behaviour, so a
//! failure here means a fix has been undone.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

#[test]
fn probe_fee_goes_where_the_caller_says() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});
    let (who, asset, shares) = holder_with(&mut svm, &p, TARGET);
    deposit(&mut svm, &p, &who, asset, shares, TARGET).expect("funds");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");
    warp(&mut svm, 100_001);
    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, TARGET);
    settle_maturity(&mut svm, &p, &p.spv, TARGET).expect("settle");

    // The holder supplies the accounts. Nothing pins `treasury_asset` to the protocol's
    // treasury — the Pool state does not even record one — so they name their own.
    let mine = create_token_account(&mut svm, &p.authority, &p.asset, &who.pubkey());
    let (position, _) = piron_single_asset_solana::seeds::position(&p.pool, &who.pubkey());
    let accounts = piron_single_asset_solana::accounts::Withdraw {
        pool: p.pool, position, vault: p.vault, share_mint: p.share_mint,
        holder_shares: shares, holder_asset: asset,
        treasury_asset: mine,
        holder: who.pubkey(),
        token_program: spl_token::ID,
    };
    let r = send(&mut svm, &who, &[], ix(p.program, accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::Withdraw { assets: TARGET }.data()));

    // 2015 is Anchor's token-owner constraint: the account does not belong to the
    // treasury the pool records, so it is refused before any money moves.
    assert!(r.is_err(), "the caller redirected the fee to themselves");
    assert_eq!(token_balance(&svm, &mine), 0);

    // And the ordinary path still pays the protocol.
    withdraw(&mut svm, &p, &who, asset, shares, TARGET).expect("withdraw");
    assert_eq!(token_balance(&svm, &p.treasury_asset), 5_000_000);
}

/// C1. Coupon entitlement is struck on subscribed shares, not on a token balance that
/// can be moved between accounts. Before the fix, 300,000,000 was paid against a
/// 200,000,000 coupon.
#[test]
fn a_coupon_cannot_be_reclaimed_by_moving_shares() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |t| {
        t.instrument_type = 1;
        t.coupon_count = 1;
        t.coupon_dates[0] = 40_000;
        t.coupon_rates[0] = 300;
    });
    let (alice, aa, a_s) = holder_with(&mut svm, &p, 500_000_000);
    let (bob, ba, b_s) = holder_with(&mut svm, &p, 500_000_000);
    deposit(&mut svm, &p, &alice, aa, a_s, 500_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, ba, b_s, 500_000_000).expect("bob");
    close_epoch(&mut svm, &p, &p.authority, true).expect("close");
    withdraw_for_investment(&mut svm, &p, &p.spv, TARGET).expect("draw");
    confirm_investment(&mut svm, &p, &p.authority, TARGET).expect("confirm");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, 400_000_000);
    record_coupon(&mut svm, &p, &p.spv, 200_000_000).expect("coupon");
    distribute_coupons(&mut svm, &p, &p.authority).expect("distribute");

    // A second coupon arrives undistributed, so an overpayment would have somewhere to
    // come from — that is where the extra 100,000,000 used to be taken from.
    svm.expire_blockhash();
    record_coupon(&mut svm, &p, &p.spv, 200_000_000).expect("second coupon");

    let vault_before = token_balance(&svm, &p.vault);
    claim_coupon(&mut svm, &p, &alice, aa, a_s).expect("alice claims");

    let ix = spl_token::instruction::transfer(
        &spl_token::ID, &a_s, &b_s, &alice.pubkey(), &[], 500_000_000).unwrap();
    send(&mut svm, &alice, &[], ix).expect("transfer");

    claim_coupon(&mut svm, &p, &bob, ba, b_s).expect("bob claims");

    let paid = vault_before - token_balance(&svm, &p.vault);
    assert_eq!(token_balance(&svm, &aa), 100_000_000, "alice's half");
    assert_eq!(token_balance(&svm, &ba), 100_000_000, "bob's half, not the whole pot");
    assert_eq!(paid, 200_000_000, "exactly the distributed coupon left the vault");
}
