//! Shared fixtures for the stable-yield tests.

#![allow(dead_code)]

use anchor_lang::{system_program, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_stable_yield_solana::seeds;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;


pub fn funded(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 100_000_000_000).unwrap();
    kp
}

pub fn send(svm: &mut LiteSVM, payer: &Keypair, signers: &[&Keypair], ix: Instruction) -> Result<(), String> {
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
pub fn token_balance(svm: &LiteSVM, account: &Pubkey) -> u64 {
    let raw = svm.get_account(account).expect("token account");
    u64::from_le_bytes(raw.data[64..72].try_into().unwrap())
}

/// Creates a mint and returns it. The authority mints freely in tests.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair, authority: &Pubkey) -> Pubkey {
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
pub fn create_token_account(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, owner: &Pubkey) -> Pubkey {
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
pub fn mint_to(svm: &mut LiteSVM, payer: &Keypair, authority: &Keypair, mint: &Pubkey, to: &Pubkey, amount: u64) {
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


pub const DECIMALS: u8 = 6;
pub const POOL_ID: u64 = 1;

pub fn ix(program: Pubkey, accounts: Vec<solana_instruction::AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: program, accounts, data }
}

pub struct Pool {
    pub program: Pubkey,
    pub authority: Keypair,
    pub spv: Keypair,
    pub treasury: Keypair,
    pub asset: Pubkey,
    pub pool: Pubkey,
    pub vault: Pubkey,
    pub share_mint: Pubkey,
    pub queue: Pubkey,
    pub spv_asset: Pubkey,
}

pub fn open_pool(svm: &mut LiteSVM, tweak: impl FnOnce(&mut (u64, i64, u32, u64, u32))) -> Pool {
    let program = piron_stable_yield_solana::ID;
    svm.add_program_from_file(program, "target/deploy/piron_stable_yield_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(svm);
    let spv = funded(svm);
    let treasury = funded(svm);
    let asset = create_mint(svm, &authority, &authority.pubkey());

    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);
    let (share_mint, _) = seeds::share_mint(&pool);

    // The queue exceeds what a CPI can allocate, so the client creates it at top level.
    let queue_kp = Keypair::new();
    let queue = queue_kp.pubkey();
    let full = piron_stable_yield_solana::state::WithdrawalQueue::LEN;
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
    let tx = Transaction::new_signed_with_payer(&[create_queue], Some(&authority.pubkey()),
        &[&authority, &queue_kp], svm.latest_blockhash());
    svm.send_transaction(tx).expect("create queue");

    // min_investment, holding_period, withdrawal_fee_bps, min_absolute_reserve, reserve_ratio_bps
    let mut cfg = (1_000u64, 0i64, 50u32, 0u64, 2_000u32);
    tweak(&mut cfg);

    let init = piron_stable_yield_solana::accounts::InitializePool {
        pool, asset_mint: asset, vault, share_mint, queue,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    send(svm, &authority, &[], ix(program, init.to_account_metas(None),
        piron_stable_yield_solana::instruction::InitializePool {
            pool_id: POOL_ID, spv: spv.pubkey(), treasury: treasury.pubkey(),
            min_investment: cfg.0, holding_period: cfg.1, withdrawal_fee_bps: cfg.2,
            min_absolute_reserve: cfg.3, reserve_ratio_bps: cfg.4,
        }.data())).expect("initialize");

    let spv_asset = create_token_account(svm, &authority, &asset, &spv.pubkey());
    Pool { program, authority, spv, treasury, asset, pool, vault, share_mint, queue, spv_asset }
}

pub fn holder_with(svm: &mut LiteSVM, p: &Pool, amount: u64) -> (Keypair, Pubkey, Pubkey) {
    let who = funded(svm);
    let a = create_token_account(svm, &p.authority, &p.asset, &who.pubkey());
    let s = create_token_account(svm, &p.authority, &p.share_mint, &who.pubkey());
    if amount > 0 { mint_to(svm, &p.authority, &p.authority, &p.asset, &a, amount); }
    (who, a, s)
}

pub fn deposit(svm: &mut LiteSVM, p: &Pool, who: &Keypair, a: Pubkey, s: Pubkey, amount: u64) -> Result<(), String> {
    let d = piron_stable_yield_solana::accounts::Deposit {
        pool: p.pool, vault: p.vault, share_mint: p.share_mint,
        holder_asset: a, holder_shares: s, holder: who.pubkey(), token_program: spl_token::ID,
    };
    send(svm, who, &[], ix(p.program, d.to_account_metas(None),
        piron_stable_yield_solana::instruction::Deposit { pool_id: POOL_ID, assets: amount }.data()))
}

pub fn deploy_capital(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, spv_asset: Pubkey, amount: u64) -> Result<(), String> {
    let d = piron_stable_yield_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset, spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_stable_yield_solana::instruction::DeployCapital { pool_id: POOL_ID, amount }.data()))
}

pub fn return_capital(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, spv_asset: Pubkey, amount: u64) -> Result<(), String> {
    let d = piron_stable_yield_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset, spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_stable_yield_solana::instruction::ReturnCapital { pool_id: POOL_ID, amount }.data()))
}

pub fn load_pool(svm: &LiteSVM, pool: &Pubkey) -> piron_stable_yield_solana::state::StableYieldPool {
    let raw = svm.get_account(pool).unwrap();
    *bytemuck::from_bytes(
        &raw.data[8..8 + core::mem::size_of::<piron_stable_yield_solana::state::StableYieldPool>()],
    )
}
