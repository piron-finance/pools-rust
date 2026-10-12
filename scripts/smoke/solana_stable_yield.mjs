// Stable-yield pool on Solana, end to end: initialize (including the queue account the
// client has to create), deposit, request a withdrawal, process it.
//
// The queue is the one place the design bends to the runtime. It needs 22,600 bytes; a CPI
// can allocate at most 10,240, and a PDA cannot sign a top-level create — so it cannot be
// both a PDA and this large. The client creates it as a plain account owned by the program
// and the pool records its address. That step is why no stable-yield pool existed on
// devnet: nothing on the client side did it.

import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction,
  SYSVAR_RENT_PUBKEY, sendAndConfirmTransaction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID, createMint, getOrCreateAssociatedTokenAccount, mintTo, getAccount,
  getMint,
} from '@solana/spl-token';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const IDL = JSON.parse(readFileSync(`${ROOT}/abi/solana/piron_stable_yield_solana.json`, 'utf8'));
const PROGRAM = new PublicKey(IDL.address);
const CLUSTER = process.env.PIRON_CLUSTER ?? 'devnet';
const conn = new Connection(process.env.PIRON_RPC ?? `https://api.${CLUSTER}.solana.com`, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(`${homedir()}/.config/solana/id.json`, 'utf8'))));

/// 8 discriminator + 64 header + 256 requests x 88 bytes.
const QUEUE_LEN = 22_600;

const disc = (n) => Buffer.from(IDL.instructions.find((i) => i.name === n).discriminator);
const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const i64 = (n) => { const b = Buffer.alloc(8); b.writeBigInt64LE(BigInt(n)); return b; };
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

const send = async (label, ixs, signers = [payer]) => {
  const list = Array.isArray(ixs) ? ixs : [ixs];
  const probe = new Transaction().add(...list);
  probe.feePayer = payer.publicKey;
  probe.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  const sim = await conn.simulateTransaction(probe);
  if (sim.value.err) {
    console.log(`  FAIL ${label}  (simulation: ${JSON.stringify(sim.value.err)})`);
    (sim.value.logs ?? []).filter((l) => /Error|error|Program log/.test(l)).slice(0, 8)
      .forEach((l) => console.log(`         ${l}`));
    process.exit(1);
  }
  // Public devnet RPC rate-limits hard; a 429 on submit is not a program failure.
  for (let i = 0; i < 8; i++) {
    try {
      const sig = await sendAndConfirmTransaction(conn, new Transaction().add(...list), signers);
      console.log(`  OK   ${label}`);
      await pause(2500);
      return sig;
    } catch (e) {
      if (e.logs?.length) {
        console.log(`  FAIL ${label}`);
        e.logs.filter((l) => /Error|Program log/.test(l)).slice(0, 8)
          .forEach((l) => console.log(`         ${l}`));
        process.exit(1);
      }
      if (i === 7) { console.log(`  FAIL ${label}: ${String(e.message).slice(0, 90)}`); process.exit(1); }
      await pause(4000 * (i + 1));
    }
  }
};

console.log(`stable-yield program ${PROGRAM.toBase58()}\n`);

const assetMint = await createMint(conn, payer, payer.publicKey, null, 6);
const holderAsset = await getOrCreateAssociatedTokenAccount(conn, payer, assetMint, payer.publicKey);
await mintTo(conn, payer, assetMint, holderAsset.address, payer, 10_000_000_000n);
const treasuryOwner = Keypair.generate();
const treasuryAsset = await getOrCreateAssociatedTokenAccount(
  conn, payer, assetMint, treasuryOwner.publicKey);
console.log(`asset mint     ${assetMint.toBase58()}`);

const poolId = BigInt(Date.now() % 1_000_000);
const [pool]      = PublicKey.findProgramAddressSync([Buffer.from('sypool'),  u64(poolId)], PROGRAM);
const [vault]     = PublicKey.findProgramAddressSync([Buffer.from('syvault'), pool.toBuffer()], PROGRAM);
const [shareMint] = PublicKey.findProgramAddressSync([Buffer.from('syshare'), pool.toBuffer()], PROGRAM);
const [holderRec] = PublicKey.findProgramAddressSync(
  [Buffer.from('syholder'), pool.toBuffer(), payer.publicKey.toBuffer()], PROGRAM);
const queue = Keypair.generate();

console.log(`pool_id ${poolId}`);
console.log(`pool           ${pool.toBase58()}`);
console.log(`vault          ${vault.toBase58()}`);
console.log(`share_mint     ${shareMint.toBase58()}`);
console.log(`queue          ${queue.publicKey.toBase58()}  (${QUEUE_LEN} bytes, client-created)\n`);

console.log(`--- create the queue account + initialize_pool ---`);
const rent = await conn.getMinimumBalanceForRentExemption(QUEUE_LEN);
console.log(`  queue rent   ${(rent / 1e9).toFixed(5)} SOL`);
const createQueue = SystemProgram.createAccount({
  fromPubkey: payer.publicKey,
  newAccountPubkey: queue.publicKey,
  lamports: rent,
  space: QUEUE_LEN,
  programId: PROGRAM,              // owned by the program, so #[account(zero)] accepts it
});

const initIx = new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: assetMint, isSigner: false, isWritable: false },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: shareMint, isSigner: false, isWritable: true },
    { pubkey: queue.publicKey, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([
    disc('initialize_pool'), u64(poolId),
    payer.publicKey.toBuffer(),         // spv
    treasuryOwner.publicKey.toBuffer(), // treasury
    u64(10_000_000),                    // min_investment
    i64(0),                             // holding_period: 0, so the queue is testable now
    u32(50),                            // withdrawal_fee_bps
    u64(0),                             // min_absolute_reserve
    u32(0),                             // reserve_ratio_bps
  ]),
});
await send('create queue + initialize_pool', [createQueue, initIx], [payer, queue]);

console.log('\n--- register_holder + deposit 5000.000000 ---');
const holderShares = await getOrCreateAssociatedTokenAccount(
  conn, payer, shareMint, payer.publicKey, false, 'confirmed', undefined, TOKEN_PROGRAM_ID);

await send('register_holder', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: false },
    { pubkey: holderRec, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('register_holder'), u64(poolId)]),
}));

await send('deposit', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: queue.publicKey, isSigner: false, isWritable: false },
    { pubkey: holderRec, isSigner: false, isWritable: true },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: shareMint, isSigner: false, isWritable: true },
    { pubkey: holderAsset.address, isSigner: false, isWritable: true },
    { pubkey: holderShares.address, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('deposit'), u64(poolId), u64(5_000_000_000)]),
}));

const show = async (label) => {
  const v = Number((await getAccount(conn, vault)).amount);
  const s = Number((await getAccount(conn, holderShares.address)).amount);
  const m = Number((await getMint(conn, shareMint)).supply);
  console.log(`${label.padEnd(22)} vault=${(v/1e6).toFixed(6)}  supply=${(m/1e6).toFixed(6)}  holder_shares=${(s/1e6).toFixed(6)}`);
};
console.log();
await show('after deposit');

console.log('\n--- request_withdrawal 2000.000000 of shares ---');
await send('request_withdrawal', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: false },
    { pubkey: queue.publicKey, isSigner: false, isWritable: true },
    { pubkey: vault, isSigner: false, isWritable: false },
    { pubkey: shareMint, isSigner: false, isWritable: true },
    { pubkey: holderShares.address, isSigner: false, isWritable: true },
    { pubkey: holderRec, isSigner: false, isWritable: false },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('request_withdrawal'), u64(poolId), u64(2_000_000_000)]),
}));
await show('after request');
console.log('  shares burn at request, so the queue is a liability against a smaller supply');

console.log('\n--- process_withdrawals ---');
const pwIx = IDL.instructions.find((i) => i.name === 'process_withdrawals');
console.log(`  accounts: ${pwIx.accounts.map((a) => a.name).join(', ')}`);
console.log(`  args:     ${pwIx.args.map((a) => a.name).join(', ')}`);
const before = Number((await getAccount(conn, holderAsset.address)).amount);
await send('process_withdrawals', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: queue.publicKey, isSigner: false, isWritable: true },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: treasuryAsset.address, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: holderAsset.address, isSigner: false, isWritable: true },
  ],
  data: Buffer.concat([disc('process_withdrawals'), u64(poolId), u32(10)]),
}));
const after = Number((await getAccount(conn, holderAsset.address)).amount);
const tre = Number((await getAccount(conn, treasuryAsset.address)).amount);
console.log();
await show('after processing');
console.log(`holder received        ${((after - before) / 1e6).toFixed(6)}`);
console.log(`treasury fee           ${(tre / 1e6).toFixed(6)}  (0.5% of 2000.000000 = 10.000000)`);
console.log(`\nPOOL: ${pool.toBase58()}`);
console.log(`QUEUE: ${queue.publicKey.toBase58()}`);
console.log(`SHARE MINT: ${shareMint.toBase58()}`);
console.log(`ASSET MINT: ${assetMint.toBase58()}`);
