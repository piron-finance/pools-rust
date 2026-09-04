//! The full single-asset arc, against an in-process SVM.
//!
//! The same scenario that was run on Stellar testnet: a raise split 60/40 between two
//! holders, settled by the SPV below face value, and both holders redeeming. The property
//! that matters is the one the Solidity got wrong — the second holder is paid their share
//! rather than finding the vault empty — so the assertions are on what each holder
//! receives and on the vault landing at zero.

use anchor_lang::{system_program, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use piron_single_asset_solana::{seeds, PoolTerms};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const DECIMALS: u8 = 6;
const TARGET: u64 = 1_000_000_000;
const POOL_ID: u64 = 1;

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

/// A raise split 60/40, settled below face value, with both holders redeeming.
///
/// This is the scenario the Solidity got wrong: the pot was recomputed from state that
/// paying it out never decremented, while the divisor shrank with every burn, so the first
/// holder took more than their share and the second found the vault empty. The assertions
/// are on what each holder receives and on the vault landing at zero.
#[test]
fn a_shortfall_is_shared_and_the_vault_lands_on_zero() {
    let mut svm = LiteSVM::new();
    let program = piron_single_asset_solana::ID;
    svm.add_program_from_file(program, "target/deploy/piron_single_asset_solana.so")
        .expect("run `cargo build-sbf` first");

    let authority = funded(&mut svm);
    let spv = funded(&mut svm);
    let treasury = funded(&mut svm);
    let alice = funded(&mut svm);
    let bob = funded(&mut svm);

    let asset = create_mint(&mut svm, &authority, &authority.pubkey());

    let (pool, _) = seeds::pool(POOL_ID);
    let (vault, _) = seeds::vault(&pool);
    let (share_mint, _) = seeds::share_mint(&pool);

    // ---- initialise ----
    let terms = PoolTerms {
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
    let accounts = piron_single_asset_solana::accounts::InitializePool {
        pool,
        asset_mint: asset,
        vault,
        share_mint,
        authority: authority.pubkey(),
        token_program: spl_token::ID,
        system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        rent: solana_pubkey::pubkey!("SysvarRent111111111111111111111111111111111"),
    };
    let data = piron_single_asset_solana::instruction::InitializePool { pool_id: POOL_ID, terms }.data();
    send(&mut svm, &authority, &[], anchor_ix(program, accounts.to_account_metas(None), data))
        .expect("initialize_pool");

    assert_eq!(token_balance(&svm, &vault), 0, "vault starts empty");

    // ---- two holders deposit 60/40 ----
    let alice_asset = create_token_account(&mut svm, &authority, &asset, &alice.pubkey());
    let bob_asset = create_token_account(&mut svm, &authority, &asset, &bob.pubkey());
    let alice_shares = create_token_account(&mut svm, &authority, &share_mint, &alice.pubkey());
    let bob_shares = create_token_account(&mut svm, &authority, &share_mint, &bob.pubkey());
    let spv_asset = create_token_account(&mut svm, &authority, &asset, &spv.pubkey());
    let treasury_asset = create_token_account(&mut svm, &authority, &asset, &treasury.pubkey());

    mint_to(&mut svm, &authority, &authority, &asset, &alice_asset, 600_000_000);
    mint_to(&mut svm, &authority, &authority, &asset, &bob_asset, 400_000_000);
    // The SPV settles below face, so it needs less than it drew; mint nothing extra.

    for (who, who_asset, who_shares, amount) in [
        (&alice, alice_asset, alice_shares, 600_000_000u64),
        (&bob, bob_asset, bob_shares, 400_000_000u64),
    ] {
        let (position, _) = seeds::position(&pool, &who.pubkey());
        let accounts = piron_single_asset_solana::accounts::Deposit {
            pool,
            position,
            vault,
            share_mint,
            investor_asset: who_asset,
            investor_shares: who_shares,
            investor: who.pubkey(),
            token_program: spl_token::ID,
            system_program: solana_pubkey::pubkey!("11111111111111111111111111111111"),
        };
        let data = piron_single_asset_solana::instruction::Deposit { assets: amount }.data();
        send(&mut svm, who, &[], anchor_ix(program, accounts.to_account_metas(None), data))
            .unwrap_or_else(|e| panic!("deposit failed: {e}"));
    }

    assert_eq!(token_balance(&svm, &vault), TARGET, "the raise is in the vault");
    assert_eq!(token_balance(&svm, &alice_shares), 600_000_000);
    assert_eq!(token_balance(&svm, &bob_shares), 400_000_000);

    // ---- close, draw, confirm ----
    let auth_accounts = piron_single_asset_solana::accounts::Authority {
        pool,
        authority: authority.pubkey(),
    };
    send(&mut svm, &authority, &[], anchor_ix(program, auth_accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::CloseEpoch { force: true }.data()))
        .unwrap_or_else(|e| panic!("close_epoch failed: {e}"));

    let draw = piron_single_asset_solana::accounts::SpvMovesFunds {
        pool, vault, spv_asset, spv: spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &spv, &[], anchor_ix(program, draw.to_account_metas(None),
        piron_single_asset_solana::instruction::WithdrawForInvestment { amount: TARGET }.data()))
        .unwrap_or_else(|e| panic!("withdraw_for_investment failed: {e}"));
    assert_eq!(token_balance(&svm, &vault), 0, "the SPV holds the money");

    send(&mut svm, &authority, &[], anchor_ix(program, auth_accounts.to_account_metas(None),
        piron_single_asset_solana::instruction::ConfirmInvestment {
            actual_amount: TARGET,
            proof: "ipfs://p".to_string(),
        }.data()))
        .unwrap_or_else(|e| panic!("confirm_investment failed: {e}"));

    // ---- settle below face ----
    // Face is 1,052,631,578 at a 5% discount. The SPV returns 900,000,000: a real loss,
    // which is exactly the case the old divisor mishandled.
    // The SPV drew the whole raise and returns less than it took, so it needs nothing
    // extra: the shortfall is the loss, not a funding gap in the test.
    let settled = 900_000_000u64;
    let mut clock = svm.get_sysvar::<anchor_lang::prelude::Clock>();
    clock.unix_timestamp = 200_000;
    svm.set_sysvar(&clock);

    let settle = piron_single_asset_solana::accounts::SpvPaysIn {
        pool, vault, share_mint, spv_asset, spv: spv.pubkey(), token_program: spl_token::ID,
    };
    send(&mut svm, &spv, &[], anchor_ix(program, settle.to_account_metas(None),
        piron_single_asset_solana::instruction::SettleMaturity { final_amount: settled }.data()))
        .unwrap_or_else(|e| panic!("settle_maturity failed: {e}"));
    assert_eq!(token_balance(&svm, &vault), settled);

    // ---- both redeem ----
    let mut received = vec![];
    for (who, who_asset, who_shares) in [
        (&alice, alice_asset, alice_shares),
        (&bob, bob_asset, bob_shares),
    ] {
        let (position, _) = seeds::position(&pool, &who.pubkey());
        let before = token_balance(&svm, &who_asset);
        let accounts = piron_single_asset_solana::accounts::Withdraw {
            pool, position, vault, share_mint,
            holder_shares: who_shares,
            holder_asset: who_asset,
            treasury_asset,
            holder: who.pubkey(),
            token_program: spl_token::ID,
        };
        send(&mut svm, who, &[], anchor_ix(program, accounts.to_account_metas(None),
            piron_single_asset_solana::instruction::Withdraw { assets: 1 }.data()))
            .unwrap_or_else(|e| panic!("withdraw failed: {e}"));
        received.push(token_balance(&svm, &who_asset) - before);
    }

    // 60% and 40% of 900,000,000, each less the 0.5% fee. The second holder is paid their
    // share rather than finding the vault empty, which is the whole point.
    assert_eq!(received[0], 537_300_000, "alice takes 60%, less the fee");
    assert_eq!(received[1], 358_200_000, "bob takes 40%, less the fee");
    assert_eq!(token_balance(&svm, &treasury_asset), 4_500_000, "fees to treasury");
    assert_eq!(token_balance(&svm, &vault), 0, "the vault lands on zero");
    assert_eq!(token_balance(&svm, &alice_shares), 0);
    assert_eq!(token_balance(&svm, &bob_shares), 0);
}
