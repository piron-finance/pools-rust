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
    pub treasury_asset: Pubkey,
}

pub fn open_pool(svm: &mut LiteSVM, tweak: impl FnOnce(&mut (u64, i64, u32, u64, u32))) -> Pool {
    let program = piron_stable_yield_solana::ID;
    assert_fresh("target/deploy/piron_stable_yield_solana.so");
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
    let treasury_asset = create_token_account(svm, &authority, &asset, &treasury.pubkey());
    Pool { program, authority, spv, treasury, asset, pool, vault, share_mint, queue, spv_asset,
           treasury_asset }
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
        pool: p.pool, queue: p.queue, holder_record: holder_record(p, &who.pubkey()),
        vault: p.vault, share_mint: p.share_mint,
        holder_asset: a, holder_shares: s, holder: who.pubkey(),
        token_program: spl_token::ID, system_program: system_program::ID,
    };
    send(svm, who, &[], ix(p.program, d.to_account_metas(None),
        piron_stable_yield_solana::instruction::Deposit { pool_id: POOL_ID, assets: amount }.data()))
}

pub fn deploy_capital(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, spv_asset: Pubkey, amount: u64) -> Result<(), String> {
    let d = piron_stable_yield_solana::accounts::SpvMovesFunds {
        pool: p.pool, queue: p.queue, vault: p.vault, spv_asset,
        spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_stable_yield_solana::instruction::DeployCapital { pool_id: POOL_ID, amount }.data()))
}

pub fn return_capital(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, spv_asset: Pubkey, amount: u64) -> Result<(), String> {
    let d = piron_stable_yield_solana::accounts::SpvMovesFunds {
        pool: p.pool, queue: p.queue, vault: p.vault, spv_asset,
        spv: caller.pubkey(), token_program: spl_token::ID,
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

pub fn add_instrument(svm: &mut LiteSVM, p: &Pool, caller: &Keypair,
                      terms: piron_stable_yield_solana::InstrumentTerms) -> Result<(), String> {
    let o = piron_stable_yield_solana::accounts::Operator { pool: p.pool, operator: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, o.to_account_metas(None),
        piron_stable_yield_solana::instruction::AddInstrument { pool_id: POOL_ID, instrument: terms }.data()))
}

pub fn discounted(purchase: u64, face: u64, matures_at: i64) -> piron_stable_yield_solana::InstrumentTerms {
    piron_stable_yield_solana::InstrumentTerms {
        instrument_type: 0, purchase_price: purchase, face_value: face,
        maturity_date: matures_at, annual_coupon_rate: 0, coupon_frequency: 0,
        next_coupon_due_date: 0,
    }
}

pub fn mature_instrument(svm: &mut LiteSVM, p: &Pool, index: u32, proceeds: u64) -> Result<(), String> {
    let m = piron_stable_yield_solana::accounts::SpvPaysIn {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(svm, &p.spv, &[], ix(p.program, m.to_account_metas(None),
        piron_stable_yield_solana::instruction::MatureInstrument {
            pool_id: POOL_ID, index, proceeds }.data()))
}

pub fn write_off(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, index: u32) -> Result<(), String> {
    let a = piron_stable_yield_solana::accounts::Authority { pool: p.pool, authority: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, a.to_account_metas(None),
        piron_stable_yield_solana::instruction::WriteOffInstrument { pool_id: POOL_ID, index }.data()))
}

pub fn holder_record(p: &Pool, owner: &Pubkey) -> Pubkey {
    piron_stable_yield_solana::seeds::holder(&p.pool, owner).0
}

pub fn request_withdrawal(svm: &mut LiteSVM, p: &Pool, who: &Keypair, shares_acc: Pubkey, shares: u64) -> Result<(), String> {
    let r = piron_stable_yield_solana::accounts::RequestWithdrawal {
        pool: p.pool, queue: p.queue, share_mint: p.share_mint,
        holder_shares: shares_acc, holder_record: holder_record(p, &who.pubkey()),
        holder: who.pubkey(), token_program: spl_token::ID,
    };
    send(svm, who, &[], ix(p.program, r.to_account_metas(None),
        piron_stable_yield_solana::instruction::RequestWithdrawal { pool_id: POOL_ID, shares }.data()))
}

pub fn load_queue(svm: &LiteSVM, queue: &Pubkey) -> (u64, u64, u64) {
    let raw = svm.get_account(queue).unwrap();
    // pool(32) then head, tail, total_pending_value
    let at = |o: usize| u64::from_le_bytes(raw.data[o..o + 8].try_into().unwrap());
    (at(8 + 32), at(8 + 40), at(8 + 48))
}

pub fn warp(svm: &mut LiteSVM, to: i64) {
    let mut clock = svm.get_sysvar::<anchor_lang::prelude::Clock>();
    clock.unix_timestamp = to;
    svm.set_sysvar(&clock);
}

/// litesvm runs the compiled artefact, not the crate `cargo test` just built, so a source
/// change with no `cargo-build-sbf` behind it would test the old program and pass. Refuse
/// to run rather than report a green suite for code that is not under test.
pub fn assert_fresh(so: &str) {
    use std::{fs, path::Path, time::SystemTime};
    let built = fs::metadata(so)
        .unwrap_or_else(|_| panic!("{so} is missing - run `cargo-build-sbf` first"))
        .modified()
        .unwrap();
    let mut newest = SystemTime::UNIX_EPOCH;
    let mut newest_path = String::new();
    let mut stack = vec![Path::new("src").to_path_buf(), Path::new("../../crates/piron-core/src").to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(t) = entry.metadata().and_then(|m| m.modified()) {
                    if t > newest {
                        newest = t;
                        newest_path = path.display().to_string();
                    }
                }
            }
        }
    }
    assert!(built >= newest, "{newest_path} is newer than {so} - run `cargo-build-sbf` first");
}

pub fn process_withdrawals(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, max_items: u32,
                           payees: &[Pubkey]) -> Result<(), String> {
    let w = piron_stable_yield_solana::accounts::ProcessWithdrawals {
        pool: p.pool, queue: p.queue, vault: p.vault, treasury_asset: p.treasury_asset,
        operator: caller.pubkey(), token_program: spl_token::ID,
    };
    let mut metas = w.to_account_metas(None);
    for payee in payees {
        metas.push(solana_instruction::AccountMeta::new(*payee, false));
    }
    send(svm, caller, &[], ix(p.program, metas,
        piron_stable_yield_solana::instruction::ProcessWithdrawals { pool_id: POOL_ID, max_items }.data()))
}

pub fn withdraw(svm: &mut LiteSVM, p: &Pool, who: &Keypair, a: Pubkey, s: Pubkey, shares: u64) -> Result<(), String> {
    let w = piron_stable_yield_solana::accounts::Withdraw {
        pool: p.pool, queue: p.queue, vault: p.vault, share_mint: p.share_mint,
        holder_asset: a, holder_shares: s, holder_record: holder_record(p, &who.pubkey()),
        treasury_asset: p.treasury_asset, holder: who.pubkey(), token_program: spl_token::ID,
    };
    send(svm, who, &[], ix(p.program, w.to_account_metas(None),
        piron_stable_yield_solana::instruction::Withdraw { pool_id: POOL_ID, shares }.data()))
}

pub fn record_coupon(svm: &mut LiteSVM, p: &Pool, index: u32, amount: u64) -> Result<(), String> {
    let m = piron_stable_yield_solana::accounts::SpvPaysIn {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: p.spv.pubkey(), token_program: spl_token::ID,
    };
    send(svm, &p.spv, &[], ix(p.program, m.to_account_metas(None),
        piron_stable_yield_solana::instruction::RecordInstrumentCoupon {
            pool_id: POOL_ID, index, amount }.data()))
}

pub fn coupon_bearing(purchase: u64, face: u64, matures_in: i64) -> piron_stable_yield_solana::InstrumentTerms {
    piron_stable_yield_solana::InstrumentTerms {
        instrument_type: 1, purchase_price: purchase, face_value: face,
        maturity_date: matures_in, annual_coupon_rate: 600, coupon_frequency: 2,
        next_coupon_due_date: matures_in / 2,
    }
}

pub fn transfer_shares(svm: &mut LiteSVM, from: &Keypair, from_acc: Pubkey, to: Pubkey, amount: u64)
    -> Result<(), String> {
    let ix = spl_token::instruction::transfer(
        &spl_token::ID, &from_acc, &to, &from.pubkey(), &[], amount).unwrap();
    send(svm, from, &[], ix)
}

/// NAV per share as the pool itself would strike it, net of what it owes the queue.
pub fn price(svm: &LiteSVM, p: &Pool) -> i128 {
    let pool = load_pool(svm, &p.pool);
    let (_, _, pending) = load_queue(svm, &p.queue);
    let supply = mint_supply(svm, &p.share_mint);
    if supply == 0 { return piron_core::stable_yield::nav::NAV_PRECISION; }
    piron_core::stable_yield::nav::nav_per_share(
        pool.total_nav as i128 - pending as i128, supply as i128).unwrap()
}

pub fn mint_supply(svm: &LiteSVM, mint: &Pubkey) -> u64 {
    let raw = svm.get_account(mint).unwrap();
    u64::from_le_bytes(raw.data[36..44].try_into().unwrap())
}
