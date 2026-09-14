//! Regression tests for the audit findings. Each asserts the corrected behaviour, so a
//! failure here means a fix has been undone.

mod harness;
use harness::*;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_signer::Signer;

#[test]
fn probe_penalty_goes_where_the_caller_says() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");
    warp_seconds(&mut svm, 43_200);

    // The holder names the account the penalty is paid into.
    let mine = create_token_account(&mut svm, &p.authority, &p.asset, &who.pubkey());
    let (position, _) = piron_locked_solana::seeds::position(&p.pool, 1);
    let e = piron_locked_solana::accounts::ExitEarly {
        pool: p.pool, position, vault: p.vault, holder_asset: acc,
        treasury_asset: mine, reserve_asset: p.reserve_asset,
        reserve: p.reserve_authority.pubkey(),
        reserve_authority: p.reserve_authority.pubkey(),
        holder: who.pubkey(), owner: who.pubkey(), token_program: spl_token::ID,
    };
    let r = send(&mut svm, &who, &[&p.reserve_authority], ix(p.program, e.to_account_metas(None),
        piron_locked_solana::instruction::ExitEarly {}.data()));

    // Refused by the token-owner constraint before any money moves.
    assert!(r.is_err(), "the caller redirected the penalty to themselves");
    assert_eq!(token_balance(&svm, &mine), 0);

    // The ordinary path still pays the protocol.
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");
    assert_eq!(token_balance(&svm, &p.treasury_asset), 50_008);
}
