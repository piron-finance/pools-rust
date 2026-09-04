//! The stable-yield arc, against an in-process SVM.
//!
//! Two things worth proving here that the other pools do not have. The queue account
//! cannot be created at its full size — a CPI caps at 10,240 bytes and 256 requests need
//! 22,600 — so it is grown by realloc and the test walks it to capacity. And a draw must
//! move value between lines rather than create any: NAV is unchanged, because cash that
//! left the vault is still the pool's.

use anchor_lang::{system_program, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_stable_yield_solana::seeds;
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
    svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?} logs={:?}", e.err, e.meta.logs))
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

fn load_pool(svm: &LiteSVM, pool: &Pubkey) -> piron_stable_yield_solana::state::StableYieldPool {
    let raw = svm.get_account(pool).unwrap();
    // Zero-copy: the account is bytes after the 8-byte discriminator.
    *bytemuck::from_bytes(&raw.data[8..8 + core::mem::size_of::<piron_stable_yield_solana::state::StableYieldPool>()])
}

#[test]
fn the_queue_grows_past_what_one_instruction_can_allocate() {
    let mut svm = LiteSVM::new();
    let program = piron_stable_yield_solana::ID;
    svm.add_program_from_file(program, "target/deploy/piron_stable_yield_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(&mut svm);
    let spv = funded(&mut svm);
    let treasury = funded(&mut svm);
    let holder = funded(&mut svm);

    let asset = create_mint(&mut svm, &authority, &authority.pubkey());
    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);
    let (share_mint, _) = seeds::share_mint(&pool);
    let queue_kp = Keypair::new();
    let queue = queue_kp.pubkey();

    // The queue exceeds what a CPI can allocate, so the client creates it at top level.
    let full = piron_stable_yield_solana::state::WithdrawalQueue::LEN;
    assert!(full > 10_240, "the queue exceeds what a CPI could have allocated");
    let rent = svm.minimum_balance_for_rent_exemption(full);
    let create_queue = Instruction {
        program_id: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        accounts: vec![
            solana_instruction::AccountMeta::new(authority.pubkey(), true),
            solana_instruction::AccountMeta::new(queue, true),
        ],
        data: {
            let mut d = vec![0, 0, 0, 0];
            d.extend_from_slice(&rent.to_le_bytes());
            d.extend_from_slice(&(full as u64).to_le_bytes());
            d.extend_from_slice(program.as_ref());
            d
        },
    };
    let tx = Transaction::new_signed_with_payer(
        &[create_queue],
        Some(&authority.pubkey()),
        &[&authority, &queue_kp],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).expect("create queue account");
    assert_eq!(svm.get_account(&queue).unwrap().data.len(), full, "created at full size");

    let init = piron_stable_yield_solana::accounts::InitializePool {
        pool, asset_mint: asset, vault, share_mint, queue,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    send(&mut svm, &authority, &[], anchor_ix(program, init.to_account_metas(None),
        piron_stable_yield_solana::instruction::InitializePool {
            pool_id: POOL_ID,
            spv: spv.pubkey(),
            treasury: treasury.pubkey(),
            min_investment: 1_000,
            holding_period: 0,
            withdrawal_fee_bps: 50,
            min_absolute_reserve: 0,
            reserve_ratio_bps: 2_000,
        }.data())).expect("initialize");

    // ---- a draw moves value between lines rather than creating any ----
    let holder_asset = create_token_account(&mut svm, &authority, &asset, &holder.pubkey());
    let holder_shares = create_token_account(&mut svm, &authority, &share_mint, &holder.pubkey());
    let spv_asset = create_token_account(&mut svm, &authority, &asset, &spv.pubkey());
    mint_to(&mut svm, &authority, &authority, &asset, &holder_asset, 1_000_000_000);

    let dep = piron_stable_yield_solana::accounts::Deposit {
        pool, vault, share_mint, holder_asset, holder_shares,
        holder: holder.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &holder, &[], anchor_ix(program, dep.to_account_metas(None),
        piron_stable_yield_solana::instruction::Deposit { pool_id: POOL_ID, assets: 1_000_000_000 }.data()))
        .expect("deposit");

    let nav_after_deposit = load_pool(&svm, &pool).total_nav;
    assert_eq!(nav_after_deposit, 1_000_000_000);
    assert_eq!(token_balance(&svm, &holder_shares), 1_000_000_000, "struck 1:1 at par");

    let draw = piron_stable_yield_solana::accounts::SpvMovesFunds {
        pool, vault, spv_asset, spv: spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &spv, &[], anchor_ix(program, draw.to_account_metas(None),
        piron_stable_yield_solana::instruction::DeployCapital { pool_id: POOL_ID, amount: 800_000_000 }.data()))
        .expect("deploy_capital");

    let after = load_pool(&svm, &pool);
    assert_eq!(after.in_flight, 800_000_000, "the draw is recorded as in flight");
    assert_eq!(token_balance(&svm, &vault), 200_000_000, "and left the vault");
    assert_eq!(
        after.total_nav, nav_after_deposit,
        "NAV is unchanged: cash with the SPV still belongs to the pool"
    );
    assert_eq!(
        token_balance(&svm, &vault) + after.in_flight,
        after.total_nav,
        "the lines sum to NAV"
    );

    // Returning more than was drawn is yield, not a negative balance.
    mint_to(&mut svm, &authority, &authority, &asset, &spv_asset, 40_000_000);
    send(&mut svm, &spv, &[], anchor_ix(program, draw.to_account_metas(None),
        piron_stable_yield_solana::instruction::ReturnCapital { pool_id: POOL_ID, amount: 840_000_000 }.data()))
        .expect("return_capital");

    let settled = load_pool(&svm, &pool);
    assert_eq!(settled.in_flight, 0, "in-flight capital is retired first");
    assert_eq!(settled.total_nav, 1_040_000_000, "the surplus lands as a NAV gain");
    assert_eq!(token_balance(&svm, &vault), settled.total_nav, "and it is all in the vault");
}
