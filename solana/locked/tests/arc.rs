//! The locked-pool arc, against an in-process SVM.
//!
//! The case worth proving is the one that failed on Stellar testnet: an early exit pays
//! principal plus interest accrued to today, and the vault holds only principal until the
//! SPV returns yield, so the exit is short by exactly the interest earned. On Stellar a
//! 500,000,000 position needed 500,000,190 and reverted by 190 stroops. Here the reserve
//! lends the difference and the pool records what it owes.

#[allow(dead_code)]
mod harness;

use anchor_lang::{system_program, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_locked_solana::seeds;
use piron_locked_solana::state::StoredTier;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;


fn funded(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 100_000_000_000).unwrap();
    kp
}

fn send(svm: &mut LiteSVM, payer: &Keypair, signers: &[&Keypair], ix: Instruction) -> Result<(), String> {
    let mut all = vec![payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &all,
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?}", e.err))
}

/// An SPL token account's `amount` sits at offset 64 in a fixed 165-byte layout: mint 32,
/// owner 32, then the amount. Read directly rather than through `Pack`, which spl-token 8
/// puts behind a feature that would drag in another Solana major.
fn token_balance(svm: &LiteSVM, account: &Pubkey) -> u64 {
    let raw = svm.get_account(account).expect("token account");
    u64::from_le_bytes(raw.data[64..72].try_into().unwrap())
}

/// Creates a mint and returns it. The authority mints freely in tests.
fn create_mint(svm: &mut LiteSVM, payer: &Keypair, authority: &Pubkey) -> Pubkey {
    let mint = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(82);
    let create = solana_instruction::Instruction {
        program_id: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        accounts: vec![
            solana_instruction::AccountMeta::new(payer.pubkey(), true),
            solana_instruction::AccountMeta::new(mint.pubkey(), true),
        ],
        data: {
            let mut d = vec![0, 0, 0, 0];
            d.extend_from_slice(&rent.to_le_bytes());
            d.extend_from_slice(&82u64.to_le_bytes());
            d.extend_from_slice(spl_token::ID.as_ref());
            d
        },
    };
    let init = spl_token::instruction::initialize_mint(
        &spl_token::ID,
        &mint.pubkey(),
        authority,
        None,
        DECIMALS,
    )
    .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create, init],
        Some(&payer.pubkey()),
        &[payer, &mint],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
    mint.pubkey()
}
fn create_token_account(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    let account = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(165);
    let create = solana_instruction::Instruction {
        program_id: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        accounts: vec![
            solana_instruction::AccountMeta::new(payer.pubkey(), true),
            solana_instruction::AccountMeta::new(account.pubkey(), true),
        ],
        data: {
            let mut d = vec![0, 0, 0, 0];
            d.extend_from_slice(&rent.to_le_bytes());
            d.extend_from_slice(&165u64.to_le_bytes());
            d.extend_from_slice(spl_token::ID.as_ref());
            d
        },
    };
    let init =
        spl_token::instruction::initialize_account(&spl_token::ID, &account.pubkey(), mint, owner)
            .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create, init],
        Some(&payer.pubkey()),
        &[payer, &account],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
    account.pubkey()
}
fn mint_to(svm: &mut LiteSVM, payer: &Keypair, authority: &Keypair, mint: &Pubkey, to: &Pubkey, amount: u64) {
    let ix = spl_token::instruction::mint_to(
        &spl_token::ID,
        mint,
        to,
        &authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    send(svm, payer, &[authority], ix).unwrap();
}

fn anchor_ix(program: Pubkey, accounts: Vec<solana_instruction::AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: program, accounts, data }
}

const DECIMALS: u8 = 6;
const POOL_ID: u64 = 1;
const PRINCIPAL: u64 = 500_000_000;

/// An early exit against a vault holding only principal.
///
/// This is the Stellar failure reproduced and fixed: the payout carries interest accrued
/// to today, the vault has only the principal, so the exit is short by exactly the interest
/// earned. Without the reserve it reverts; with it the pool borrows the difference and
/// records the debt, and the SPV settles from the capital it returns.
#[test]
fn an_early_exit_is_covered_by_the_reserve() {
    let mut svm = LiteSVM::new();
    let program = piron_locked_solana::ID;
    harness::assert_fresh("target/deploy/piron_locked_solana.so");
    svm.add_program_from_file(program, "target/deploy/piron_locked_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(&mut svm);
    let spv = funded(&mut svm);
    let treasury = funded(&mut svm);
    let holder = funded(&mut svm);
    let reserve_authority = funded(&mut svm);

    let asset = create_mint(&mut svm, &authority, &authority.pubkey());
    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);

    let holder_asset = create_token_account(&mut svm, &authority, &asset, &holder.pubkey());
    let treasury_asset = create_token_account(&mut svm, &authority, &asset, &treasury.pubkey());
    let spv_asset = create_token_account(&mut svm, &authority, &asset, &spv.pubkey());
    let reserve_asset = create_token_account(&mut svm, &authority, &asset, &reserve_authority.pubkey());
    mint_to(&mut svm, &authority, &authority, &asset, &holder_asset, PRINCIPAL);
    mint_to(&mut svm, &authority, &authority, &asset, &reserve_asset, 100_000_000);

    let init = piron_locked_solana::accounts::InitializePool {
        pool, asset_mint: asset, vault,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    send(&mut svm, &authority, &[], anchor_ix(program, init.to_account_metas(None),
        piron_locked_solana::instruction::InitializePool {
            pool_id: POOL_ID,
            spv: spv.pubkey(),
            treasury: treasury.pubkey(),
            reserve: reserve_authority.pubkey(),
            min_investment: 1_000,
        }.data())).expect("initialize");

    // 1 day, 12% APY, 5% early-exit penalty.
    let auth = piron_locked_solana::accounts::Authority { pool, authority: authority.pubkey() };
    send(&mut svm, &authority, &[], anchor_ix(program, auth.to_account_metas(None),
        piron_locked_solana::instruction::AddTier {
            tier: StoredTier {
                duration_days: 1, apy_bps: 1_200, early_exit_penalty_bps: 500,
                min_deposit: 1_000, is_active: true,
            },
        }.data())).expect("add_tier");

    let (position, _) = seeds::position(&pool, 1);
    let dep = piron_locked_solana::accounts::Deposit {
        pool, position, vault, holder_asset,
        holder: holder.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
    };
    send(&mut svm, &holder, &[], anchor_ix(program, dep.to_account_metas(None),
        piron_locked_solana::instruction::Deposit {
            amount: PRINCIPAL, tier_index: 0, payment_choice: 1,
        }.data())).expect("deposit");

    assert_eq!(token_balance(&svm, &vault), PRINCIPAL, "the vault holds only principal");

    // Half way through the term, so there is accrued interest the vault cannot cover.
    let mut clock = svm.get_sysvar::<anchor_lang::prelude::Clock>();
    clock.unix_timestamp += 43_200;
    svm.set_sysvar(&clock);

    let before = token_balance(&svm, &holder_asset);
    let exit = piron_locked_solana::accounts::ExitEarly {
        pool, position, vault, holder_asset, treasury_asset, reserve_asset,
        reserve: reserve_authority.pubkey(),
        reserve_authority: reserve_authority.pubkey(),
        holder: holder.pubkey(),
        owner: holder.pubkey(),
        token_program: spl_token::ID,
    };
    send(&mut svm, &holder, &[&reserve_authority], anchor_ix(program, exit.to_account_metas(None),
        piron_locked_solana::instruction::ExitEarly {}.data())).expect("exit_early");

    let received = token_balance(&svm, &holder_asset) - before;
    let penalty = token_balance(&svm, &treasury_asset);
    let debt = {
        let raw = svm.get_account(&pool).unwrap();
        let acc: piron_locked_solana::state::LockedPool =
            anchor_lang::AccountDeserialize::try_deserialize(&mut raw.data.as_slice()).unwrap();
        acc.reserve_debt
    };

    // The payout carries half a day of interest, so it exceeds principal less the penalty.
    assert!(received > 0, "the holder is paid");
    assert!(penalty > 0, "the penalty leaves the pool");
    assert_eq!(
        received + penalty,
        PRINCIPAL + debt,
        "every unit paid out came from the vault or was borrowed"
    );
    assert!(debt > 0, "the shortfall was borrowed rather than conjured");
    assert_eq!(token_balance(&svm, &vault), 0, "the vault is drained exactly");

    // The SPV settles out of the capital it returned.
    mint_to(&mut svm, &authority, &authority, &asset, &spv_asset, debt);
    let settle = piron_locked_solana::accounts::SettleDebt {
        pool, spv_asset, reserve_asset, spv: spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &spv, &[], anchor_ix(program, settle.to_account_metas(None),
        piron_locked_solana::instruction::SettleDebt { amount: debt }.data())).expect("settle_debt");

    let after = {
        let raw = svm.get_account(&pool).unwrap();
        let acc: piron_locked_solana::state::LockedPool =
            anchor_lang::AccountDeserialize::try_deserialize(&mut raw.data.as_slice()).unwrap();
        acc.reserve_debt
    };
    assert_eq!(after, 0, "the debt is cleared");
}
