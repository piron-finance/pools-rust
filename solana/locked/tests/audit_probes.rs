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

/// SOL-1. The reserve's token account is bound to the reserve the pool records, on both
/// the borrow and the repay path. Before the fix `SettleDebt.reserve_asset` carried only a
/// mint constraint, so the SPV could clear the pool's debt by paying an account it
/// controlled — the books showed the reserve repaid while the reserve received nothing.
#[test]
fn debt_can_only_be_repaid_to_the_reserve() {
    let mut svm = LiteSVM::new();
    let p = open_pool(&mut svm, 5_000_000);
    add_tier(&mut svm, &p, &p.authority, standard_tier()).expect("tier");
    let (who, acc) = holder_with(&mut svm, &p, 1_000_000);
    deposit(&mut svm, &p, &who, acc, 1_000_000, 0, 1, 1).expect("deposit");

    // Half a day in, the vault holds principal only, so the exit borrows.
    warp_seconds(&mut svm, 43_200);
    exit_early(&mut svm, &p, &who, acc, 1).expect("exit");
    let debt = load_pool(&svm, &p.pool).reserve_debt;
    assert!(debt > 0, "nothing was borrowed");

    mint_to(&mut svm, &p.authority, &p.authority, &p.asset, &p.spv_asset, debt);

    // The SPV names its own account as the reserve.
    let mine = create_token_account(&mut svm, &p.authority, &p.asset, &p.spv.pubkey());
    let d = piron_locked_solana::accounts::SettleDebt {
        pool: p.pool, spv_asset: p.spv_asset, reserve_asset: mine,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    let r = send(&mut svm, &p.spv, &[], ix(p.program, d.to_account_metas(None),
        piron_locked_solana::instruction::SettleDebt { amount: debt }.data()));

    assert!(r.is_err(), "the SPV repaid the pool's debt to itself");
    assert_eq!(load_pool(&svm, &p.pool).reserve_debt, debt, "debt was cleared anyway");

    // Repaying the real reserve works and clears it.
    let reserve_before = token_balance(&svm, &p.reserve_asset);
    settle_debt(&mut svm, &p, &p.spv, debt).expect("repay the reserve");
    assert_eq!(token_balance(&svm, &p.reserve_asset) - reserve_before, debt);
    assert_eq!(load_pool(&svm, &p.pool).reserve_debt, 0);
}
