//! The full single-asset arc.
//!
//! The scenario that ran on Stellar testnet: a raise split 60/40 between two holders,
//! settled by the SPV below face value, and both redeeming. The property that matters is
//! the one the Solidity got wrong — the second holder is paid their share rather than
//! finding the vault empty.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

#[test]
fn a_shortfall_is_shared_and_the_vault_lands_on_zero() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, |_| {});

    let (alice, alice_asset, alice_shares) = holder_with(&mut svm, &p, 600_000_000);
    let (bob, bob_asset, bob_shares) = holder_with(&mut svm, &p, 400_000_000);

    deposit(&mut svm, &p, &alice, alice_asset, alice_shares, 600_000_000).expect("alice");
    deposit(&mut svm, &p, &bob, bob_asset, bob_shares, 400_000_000).expect("bob");
    assert_eq!(token_balance(&svm, &p.vault), TARGET, "the raise is in the vault");

    close_epoch(&mut svm, &p, &p.authority, true).expect("close");

    let draw = piron_single_asset_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, draw.to_account_metas(None),
        piron_single_asset_solana::instruction::WithdrawForInvestment { amount: TARGET }.data()))
        .expect("draw");
    assert_eq!(token_balance(&svm, &p.vault), 0, "the SPV holds the money");

    let auth = piron_single_asset_solana::accounts::Authority { pool: p.pool, authority: p.authority.pubkey() };
    send(&mut svm, &p.authority, &[], ix(p.program, auth.to_account_metas(None),
        piron_single_asset_solana::instruction::ConfirmInvestment {
            actual_amount: TARGET, proof: "ipfs://p".to_string(),
        }.data())).expect("confirm");

    // Face is 1,052,631,578 at a 5% discount. The SPV returns 900,000,000: a real loss.
    warp(&mut svm, 200_000);
    let settle = piron_single_asset_solana::accounts::SpvPaysIn {
        pool: p.pool, vault: p.vault, share_mint: p.share_mint, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &p.spv, &[], ix(p.program, settle.to_account_metas(None),
        piron_single_asset_solana::instruction::SettleMaturity { final_amount: 900_000_000 }.data()))
        .expect("settle");

    let a0 = token_balance(&svm, &alice_asset);
    let b0 = token_balance(&svm, &bob_asset);
    withdraw(&mut svm, &p, &alice, alice_asset, alice_shares, 1).expect("alice redeems");
    withdraw(&mut svm, &p, &bob, bob_asset, bob_shares, 1).expect("bob redeems");

    assert_eq!(token_balance(&svm, &alice_asset) - a0, 537_300_000, "60%, less the fee");
    assert_eq!(token_balance(&svm, &bob_asset) - b0, 358_200_000, "40%, less the fee");
    assert_eq!(token_balance(&svm, &p.treasury_asset), 4_500_000, "fees to treasury");
    assert_eq!(token_balance(&svm, &p.vault), 0, "the vault lands on zero");
    assert_eq!(token_balance(&svm, &alice_shares), 0);
    assert_eq!(token_balance(&svm, &bob_shares), 0);
}
