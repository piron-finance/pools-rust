// End-to-end on devnet: create a pool through the deployed single-asset program and
// deposit into it. Encodes instructions by hand from the published IDL, which is also the
// point — if the IDL in abi/solana/ is wrong, this fails.
//
// This is the test that was never run. "solana program deploy exited 0" is not evidence
// that a program works; a transaction that lands is.

import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction,
  SYSVAR_RENT_PUBKEY, sendAndConfirmTransaction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID, createMint, getOrCreateAssociatedTokenAccount, mintTo, getAccount,
} from '@solana/spl-token';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const IDL = JSON.parse(readFileSync(
  `${ROOT}/abi/solana/piron_single_asset_solana.json`, 'utf8'));
const PROGRAM = new PublicKey(IDL.address);

const CLUSTER = process.env.PIRON_CLUSTER ?? 'devnet';
const conn = new Connection(
  process.env.PIRON_RPC ?? `https://api.${CLUSTER}.solana.com`, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(`${homedir()}/.config/solana/id.json`, 'utf8'))));

const disc = (name) =>
  Buffer.from(IDL.instructions.find((i) => i.name === name).discriminator);

const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const i64 = (n) => { const b = Buffer.alloc(8); b.writeBigInt64LE(BigInt(n)); return b; };
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const u8  = (n) => Buffer.from([n]);

console.log(`program ${PROGRAM.toBase58()} (from the published IDL)\n`);

// ---------------------------------------------------------------- the asset
const assetMint = await createMint(conn, payer, payer.publicKey, null, 6);
console.log(`asset mint            ${assetMint.toBase58()}  (6dp, mock USDC)`);

const investorAsset = await getOrCreateAssociatedTokenAccount(
  conn, payer, assetMint, payer.publicKey);
await mintTo(conn, payer, assetMint, investorAsset.address, payer, 10_000_000_000n);
console.log(`investor funded       10,000.000000`);

// ---------------------------------------------------------------- PDAs
const poolId = BigInt(Date.now() % 1_000_000);
const [pool] = PublicKey.findProgramAddressSync(
  [Buffer.from('pool'), u64(poolId)], PROGRAM);
const [vault] = PublicKey.findProgramAddressSync(
  [Buffer.from('vault'), pool.toBuffer()], PROGRAM);
const [shareMint] = PublicKey.findProgramAddressSync(
  [Buffer.from('share_mint'), pool.toBuffer()], PROGRAM);
const [position] = PublicKey.findProgramAddressSync(
  [Buffer.from('position'), pool.toBuffer(), payer.publicKey.toBuffer()], PROGRAM);

console.log(`\npool_id ${poolId}`);
console.log(`pool                  ${pool.toBase58()}`);
console.log(`vault                 ${vault.toBase58()}`);
console.log(`share_mint            ${shareMint.toBase58()}`);

// ---------------------------------------------------------------- initialize_pool
const now = Math.floor(Date.now() / 1000);
const terms = Buffer.concat([
  payer.publicKey.toBuffer(),              // spv
  payer.publicKey.toBuffer(),              // treasury
  u8(0),                                   // instrument_type: discounted
  u64(1_000_000_000),                      // purchase_price  1,000.000000
  u64(1_000_000_000),                      // target_raise    1,000.000000
  i64(now + 7 * 86400),                    // epoch_end_time
  i64(now + 90 * 86400),                   // maturity_date
  u8(0),                                   // coupon_count
  Buffer.alloc(8 * 24),                    // coupon_dates[24]
  Buffer.alloc(4 * 24),                    // coupon_rates[24]
  u32(500),                                // discount_rate 5%
  u32(8000),                               // minimum_funding_threshold 80%
  u64(10_000_000),                         // min_investment 10.000000
  u32(50),                                 // withdrawal_fee_bps
]);

const initIx = new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool,              isSigner: false, isWritable: true  },
    { pubkey: assetMint,         isSigner: false, isWritable: false },
    { pubkey: vault,             isSigner: false, isWritable: true  },
    { pubkey: shareMint,         isSigner: false, isWritable: true  },
    { pubkey: payer.publicKey,   isSigner: true,  isWritable: true  },
    { pubkey: TOKEN_PROGRAM_ID,  isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('initialize_pool'), u64(poolId), terms]),
});

console.log('\n--- initialize_pool ---');
try {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(initIx), [payer]);
  console.log(`OK  ${sig}`);
} catch (e) {
  console.log('FAILED');
  (e.logs ?? []).filter(l => /Error|error|Program log/.test(l)).slice(0, 8).forEach(l => console.log(`    ${l}`));
  process.exit(1);
}

// ---------------------------------------------------------------- deposit
const investorShares = await getOrCreateAssociatedTokenAccount(
  conn, payer, shareMint, payer.publicKey, false, 'confirmed', undefined, TOKEN_PROGRAM_ID);

const depositIx = new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool,                     isSigner: false, isWritable: true  },
    { pubkey: position,                 isSigner: false, isWritable: true  },
    { pubkey: vault,                    isSigner: false, isWritable: true  },
    { pubkey: shareMint,                isSigner: false, isWritable: true  },
    { pubkey: investorAsset.address,    isSigner: false, isWritable: true  },
    { pubkey: investorShares.address,   isSigner: false, isWritable: true  },
    { pubkey: payer.publicKey,          isSigner: true,  isWritable: true  },
    { pubkey: TOKEN_PROGRAM_ID,         isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId,  isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('deposit'), u64(500_000_000)]),   // 500.000000
});

console.log('\n--- deposit 500.000000 ---');
try {
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(depositIx), [payer]);
  console.log(`OK  ${sig}`);
} catch (e) {
  console.log('FAILED');
  (e.logs ?? []).filter(l => /Error|error|Program log/.test(l)).slice(0, 8).forEach(l => console.log(`    ${l}`));
  process.exit(1);
}

// ---------------------------------------------------------------- verify
const v = await getAccount(conn, vault);
const s = await getAccount(conn, investorShares.address);
const a = await getAccount(conn, investorAsset.address);
console.log('\n--- state after ---');
console.log(`vault holds           ${(Number(v.amount) / 1e6).toFixed(6)}`);
console.log(`investor shares       ${(Number(s.amount) / 1e6).toFixed(6)}`);
console.log(`investor asset left   ${(Number(a.amount) / 1e6).toFixed(6)}`);

const poolAcct = await conn.getAccountInfo(pool);
console.log(`pool account          ${poolAcct.data.length} bytes, owner ${poolAcct.owner.toBase58()}`);
console.log(`\nPOOL: ${pool.toBase58()}`);
console.log(`ASSET MINT: ${assetMint.toBase58()}`);
console.log(`SHARE MINT: ${shareMint.toBase58()}`);
console.log(`VAULT: ${vault.toBase58()}`);
