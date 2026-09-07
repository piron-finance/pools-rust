//! Shared fixtures for the single-asset tests.
//!
//! `open_pool` gives a pool in FUNDING with sane defaults and one hook to vary whatever
//! the test is actually about, so a test that is about the minimum investment says so and
//! nothing else.

#![allow(dead_code)]

use anchor_lang::{system_program, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_single_asset_solana::{seeds, PoolTerms};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

pub const DECIMALS: u8 = 6;
pub const TARGET: u64 = 1_000_000_000;

struct World {
    svm: LiteSVM,
    program: Pubkey,
    authority: Keypair,
    spv: Keypair,
    treasury: Keypair,
    asset_mint: Pubkey,
    pool: Pubkey,
    vault: Pubkey,
    share_mint: Pubkey,
}

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
    svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?}", e.err))
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



pub struct Pool {
    pub program: Pubkey,
    pub authority: Keypair,
    pub spv: Keypair,
    pub treasury: Keypair,
    pub asset: Pubkey,
    pub pool: Pubkey,
    pub vault: Pubkey,
    pub share_mint: Pubkey,
    pub treasury_asset: Pubkey,
    pub spv_asset: Pubkey,
}

pub const POOL_ID: u64 = 1;

/// A pool in FUNDING with nothing raised. `terms` is handed the defaults so a test can
/// vary the one thing it is about.
pub fn open_pool(svm: &mut LiteSVM, tweak: impl FnOnce(&mut PoolTerms)) -> Pool {
    let program = piron_single_asset_solana::ID;
    svm.add_program_from_file(program, "target/deploy/piron_single_asset_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(svm);
    let spv = funded(svm);
    let treasury = funded(svm);
    let asset = create_mint(svm, &authority, &authority.pubkey());

    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);
    let (share_mint, _) = seeds::share_mint(&pool);

    let mut terms = PoolTerms {
        spv: spv.pubkey(),
        instrument_type: 0,
        purchase_price: TARGET,
        target_raise: TARGET,
        epoch_end_time: 1_000,
        maturity_date: 100_000,
        coupon_count: 0,
        coupon_dates: [0; piron_core::MAX_COUPON_SCHEDULE],
        coupon_rates: [0; piron_core::MAX_COUPON_SCHEDULE],
        discount_rate: 500,
        minimum_funding_threshold: 8_000,
        min_investment: 1_000,
        withdrawal_fee_bps: 50,
    };
    tweak(&mut terms);

    let accounts = piron_single_asset_solana::accounts::InitializePool {
        pool, asset_mint: asset, vault, share_mint,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    let data = piron_single_asset_solana::instruction::InitializePool { pool_id: POOL_ID, terms }.data();
    send(svm, &authority, &[], ix(program, accounts.to_account_metas(None), data))
        .expect("initialize_pool");

    let treasury_asset = create_token_account(svm, &authority, &asset, &treasury.pubkey());
    let spv_asset = create_token_account(svm, &authority, &asset, &spv.pubkey());
    Pool { program, authority, spv, treasury, asset, pool, vault, share_mint, treasury_asset, spv_asset }
}

/// A holder with `amount` of the asset and an empty share account.
pub fn holder_with(svm: &mut LiteSVM, p: &Pool, amount: u64) -> (Keypair, Pubkey, Pubkey) {
    let who = funded(svm);
    let asset_acc = create_token_account(svm, &p.authority, &p.asset, &who.pubkey());
    let share_acc = create_token_account(svm, &p.authority, &p.share_mint, &who.pubkey());
    if amount > 0 {
        mint_to(svm, &p.authority, &p.authority, &p.asset, &asset_acc, amount);
    }
    (who, asset_acc, share_acc)
}

pub fn deposit(svm: &mut LiteSVM, p: &Pool, who: &Keypair, asset_acc: Pubkey, share_acc: Pubkey, amount: u64) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, &who.pubkey());
    let accounts = piron_single_asset_solana::accounts::Deposit {
        pool: p.pool, position, vault: p.vault, share_mint: p.share_mint,
        investor_asset: asset_acc, investor_shares: share_acc,
        investor: who.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
    };
    send(svm, who, &[], ix(p.program, accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::Deposit { assets: amount }.data()))
}

pub fn close_epoch(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, force: bool) -> Result<(), String> {
    let accounts = piron_single_asset_solana::accounts::Authority { pool: p.pool, authority: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::CloseEpoch { force }.data()))
}

pub fn withdraw(svm: &mut LiteSVM, p: &Pool, who: &Keypair, asset_acc: Pubkey, share_acc: Pubkey, assets: u64) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, &who.pubkey());
    let accounts = piron_single_asset_solana::accounts::Withdraw {
        pool: p.pool, position, vault: p.vault, share_mint: p.share_mint,
        holder_shares: share_acc, holder_asset: asset_acc,
        treasury_asset: p.treasury_asset,
        holder: who.pubkey(),
        token_program: spl_token::ID,
    };
    send(svm, who, &[], ix(p.program, accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::Withdraw { assets }.data()))
}

pub fn warp(svm: &mut LiteSVM, to: i64) {
    let mut clock = svm.get_sysvar::<anchor_lang::prelude::Clock>();
    clock.unix_timestamp = to;
    svm.set_sysvar(&clock);
}

pub fn ix(program: Pubkey, accounts: Vec<solana_instruction::AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: program, accounts, data }
}
