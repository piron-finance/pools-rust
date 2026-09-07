//! Shared fixtures for the locked-pool tests.
//!
//! `open_pool` gives a pool with one tier and a funded reserve, so a test that is about
//! the tier ceiling says so and nothing else.

#![allow(dead_code)]

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

fn anchor_ix(program: Pubkey, accounts: Vec<solana_instruction::AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: program, accounts, data }
}


pub const DECIMALS: u8 = 6;
pub const POOL_ID: u64 = 1;
pub const PRINCIPAL: u64 = 500_000_000;

pub fn ix(program: Pubkey, accounts: Vec<solana_instruction::AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: program, accounts, data }
}

pub struct Pool {
    pub program: Pubkey,
    pub authority: Keypair,
    pub spv: Keypair,
    pub treasury: Keypair,
    pub reserve_authority: Keypair,
    pub asset: Pubkey,
    pub pool: Pubkey,
    pub vault: Pubkey,
    pub treasury_asset: Pubkey,
    pub spv_asset: Pubkey,
    pub reserve_asset: Pubkey,
}

pub fn open_pool(svm: &mut LiteSVM, reserve_funding: u64) -> Pool {
    let program = piron_locked_solana::ID;
    assert_fresh("target/deploy/piron_locked_solana.so");
    svm.add_program_from_file(program, "target/deploy/piron_locked_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(svm);
    let spv = funded(svm);
    let treasury = funded(svm);
    let reserve_authority = funded(svm);
    let asset = create_mint(svm, &authority, &authority.pubkey());

    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);

    let treasury_asset = create_token_account(svm, &authority, &asset, &treasury.pubkey());
    let spv_asset = create_token_account(svm, &authority, &asset, &spv.pubkey());
    let reserve_asset = create_token_account(svm, &authority, &asset, &reserve_authority.pubkey());
    if reserve_funding > 0 {
        mint_to(svm, &authority, &authority, &asset, &reserve_asset, reserve_funding);
    }

    let init = piron_locked_solana::accounts::InitializePool {
        pool, asset_mint: asset, vault,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    send(svm, &authority, &[], ix(program, init.to_account_metas(None),
        piron_locked_solana::instruction::InitializePool {
            pool_id: POOL_ID,
            spv: spv.pubkey(),
            treasury: treasury.pubkey(),
            reserve: reserve_authority.pubkey(),
            min_investment: 1_000,
        }.data())).expect("initialize");

    Pool { program, authority, spv, treasury, reserve_authority, asset, pool, vault,
           treasury_asset, spv_asset, reserve_asset }
}

pub fn add_tier(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, tier: StoredTier) -> Result<(), String> {
    let auth = piron_locked_solana::accounts::Authority { pool: p.pool, authority: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, auth.to_account_metas(None),
        piron_locked_solana::instruction::AddTier { tier }.data()))
}

pub fn standard_tier() -> StoredTier {
    StoredTier { duration_days: 1, apy_bps: 1_200, early_exit_penalty_bps: 500,
                 min_deposit: 1_000, is_active: true }
}

pub fn holder_with(svm: &mut LiteSVM, p: &Pool, amount: u64) -> (Keypair, Pubkey) {
    let who = funded(svm);
    let acc = create_token_account(svm, &p.authority, &p.asset, &who.pubkey());
    if amount > 0 { mint_to(svm, &p.authority, &p.authority, &p.asset, &acc, amount); }
    (who, acc)
}

pub fn deposit(svm: &mut LiteSVM, p: &Pool, who: &Keypair, acc: Pubkey, amount: u64,
               tier_index: u32, payment_choice: u8, position_id: u64) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, position_id);
    let d = piron_locked_solana::accounts::Deposit {
        pool: p.pool, position, vault: p.vault, holder_asset: acc,
        holder: who.pubkey(), token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
    };
    send(svm, who, &[], ix(p.program, d.to_account_metas(None),
        piron_locked_solana::instruction::Deposit { amount, tier_index, payment_choice }.data()))
}

pub fn redeem(svm: &mut LiteSVM, p: &Pool, who: &Keypair, acc: Pubkey, position_id: u64) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, position_id);
    let s = piron_locked_solana::accounts::Settle {
        pool: p.pool, position, vault: p.vault, holder_asset: acc,
        holder: who.pubkey(), owner: who.pubkey(), token_program: spl_token::ID,
    };
    send(svm, who, &[], ix(p.program, s.to_account_metas(None),
        piron_locked_solana::instruction::Redeem {}.data()))
}

pub fn exit_early(svm: &mut LiteSVM, p: &Pool, who: &Keypair, acc: Pubkey, position_id: u64) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, position_id);
    let e = piron_locked_solana::accounts::ExitEarly {
        pool: p.pool, position, vault: p.vault, holder_asset: acc,
        treasury_asset: p.treasury_asset, reserve_asset: p.reserve_asset,
        reserve: p.reserve_authority.pubkey(),
        reserve_authority: p.reserve_authority.pubkey(),
        holder: who.pubkey(), owner: who.pubkey(), token_program: spl_token::ID,
    };
    send(svm, who, &[&p.reserve_authority], ix(p.program, e.to_account_metas(None),
        piron_locked_solana::instruction::ExitEarly {}.data()))
}

pub fn load_pool(svm: &LiteSVM, pool: &Pubkey) -> piron_locked_solana::state::LockedPool {
    let raw = svm.get_account(pool).unwrap();
    anchor_lang::AccountDeserialize::try_deserialize(&mut raw.data.as_slice()).unwrap()
}

pub fn warp_days(svm: &mut LiteSVM, days: i64) {
    warp_seconds(svm, days * 86_400);
}

pub fn warp_seconds(svm: &mut LiteSVM, seconds: i64) {
    let mut clock = svm.get_sysvar::<anchor_lang::prelude::Clock>();
    clock.unix_timestamp += seconds;
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

pub fn roll_over(svm: &mut LiteSVM, p: &Pool, who: &Keypair, acc: Pubkey, position_id: u64,
                 new_id: u64, tier_index: u32, payment_choice: u8) -> Result<(), String> {
    let (position, _) = seeds::position(&p.pool, position_id);
    let (new_position, _) = seeds::position(&p.pool, new_id);
    let r = piron_locked_solana::accounts::RollOver {
        pool: p.pool, position, new_position, vault: p.vault, holder_asset: acc,
        holder: who.pubkey(), owner: who.pubkey(), token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
    };
    send(svm, who, &[], ix(p.program, r.to_account_metas(None),
        piron_locked_solana::instruction::RollOver { tier_index, payment_choice }.data()))
}

pub fn load_position(svm: &LiteSVM, p: &Pool, position_id: u64) -> piron_locked_solana::state::LockedPosition {
    let (position, _) = seeds::position(&p.pool, position_id);
    let raw = svm.get_account(&position).unwrap();
    anchor_lang::AccountDeserialize::try_deserialize(&mut &raw.data[..]).unwrap()
}

pub fn deploy_to_spv(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, amount: u64) -> Result<(), String> {
    let d = piron_locked_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_locked_solana::instruction::DeployToSpv { amount }.data()))
}

pub fn return_from_spv(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, amount: u64) -> Result<(), String> {
    let d = piron_locked_solana::accounts::SpvMovesFunds {
        pool: p.pool, vault: p.vault, spv_asset: p.spv_asset,
        spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_locked_solana::instruction::ReturnFromSpv { amount }.data()))
}

pub fn settle_debt(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, amount: u64) -> Result<(), String> {
    let d = piron_locked_solana::accounts::SettleDebt {
        pool: p.pool, spv_asset: p.spv_asset, reserve_asset: p.reserve_asset,
        spv: caller.pubkey(), token_program: spl_token::ID,
    };
    send(svm, caller, &[], ix(p.program, d.to_account_metas(None),
        piron_locked_solana::instruction::SettleDebt { amount }.data()))
}

pub fn set_tier_active(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, tier_index: u32, is_active: bool)
    -> Result<(), String> {
    let auth = piron_locked_solana::accounts::Authority { pool: p.pool, authority: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, auth.to_account_metas(None),
        piron_locked_solana::instruction::SetTierActive { tier_index, is_active }.data()))
}

pub fn set_open(svm: &mut LiteSVM, p: &Pool, caller: &Keypair, is_open: bool) -> Result<(), String> {
    let auth = piron_locked_solana::accounts::Authority { pool: p.pool, authority: caller.pubkey() };
    send(svm, caller, &[], ix(p.program, auth.to_account_metas(None),
        piron_locked_solana::instruction::SetOpen { is_open }.data()))
}
