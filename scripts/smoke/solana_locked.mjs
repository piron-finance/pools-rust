// Locked pool on Solana, end to end: initialize, add a tier, deposit, exit early.
//
// Instructions are encoded by hand from the published IDL, so a wrong IDL fails here
// rather than being papered over by a client generated from the same file.

import {
  Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction,
  SYSVAR_RENT_PUBKEY, sendAndConfirmTransaction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID, createMint, getOrCreateAssociatedTokenAccount, mintTo, getAccount,
} from '@solana/spl-token';
import { readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const IDL = JSON.parse(readFileSync(`${ROOT}/abi/solana/piron_locked_solana.json`, 'utf8'));
const PROGRAM = new PublicKey(IDL.address);
const CLUSTER = process.env.PIRON_CLUSTER ?? 'devnet';
const conn = new Connection(process.env.PIRON_RPC ?? `https://api.${CLUSTER}.solana.com`, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(readFileSync(`${homedir()}/.config/solana/id.json`, 'utf8'))));

const disc = (n) => Buffer.from(IDL.instructions.find((i) => i.name === n).discriminator);
const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const i64 = (n) => { const b = Buffer.alloc(8); b.writeBigInt64LE(BigInt(n)); return b; };
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const u8  = (n) => Buffer.from([n]);
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const send = async (label, ix, signers = [payer]) => {
  // Simulate first and keep the logs. A 429 during send retry otherwise discards the
  // program's own error, which is the only part worth reading.
  const tx = new Transaction().add(ix);
  tx.feePayer = payer.publicKey;
  tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  const sim = await conn.simulateTransaction(tx);
  if (sim.value.err) {
    console.log(`  FAIL ${label}  (simulation)`);
    (sim.value.logs ?? []).filter((l) => /Error|error|Program log/.test(l)).slice(0, 8)
      .forEach((l) => console.log(`         ${l}`));
    process.exit(1);
  }
  for (let attempt = 0; attempt < 8; attempt++) {
    try {
      const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), signers);
      console.log(`  OK   ${label}`);
      await pause(2500);
      return sig;
    } catch (e) {
      if (attempt === 7) {
        console.log(`  FAIL ${label}`);
        (e.logs ?? []).filter((l) => /Error|error|Program log/.test(l)).slice(0, 8)
          .forEach((l) => console.log(`         ${l}`));
        process.exit(1);
      }
      await pause(4000 * (attempt + 1));
    }
  }
};

console.log(`locked program ${PROGRAM.toBase58()}\n`);

const assetMint = await createMint(conn, payer, payer.publicKey, null, 6);
const holderAsset = await getOrCreateAssociatedTokenAccount(conn, payer, assetMint, payer.publicKey);
await mintTo(conn, payer, assetMint, holderAsset.address, payer, 10_000_000_000n);
const treasuryOwner = Keypair.generate();
const treasuryAsset = await getOrCreateAssociatedTokenAccount(
  conn, payer, assetMint, treasuryOwner.publicKey);
const reserveOwner = Keypair.generate();
const reserveAsset = await getOrCreateAssociatedTokenAccount(
  conn, payer, assetMint, reserveOwner.publicKey);
await mintTo(conn, payer, assetMint, reserveAsset.address, payer, 5_000_000_000n);
console.log(`asset mint     ${assetMint.toBase58()}`);

const poolId = BigInt(Date.now() % 1_000_000);
const [pool]  = PublicKey.findProgramAddressSync([Buffer.from('lpool'),  u64(poolId)], PROGRAM);
const [vault] = PublicKey.findProgramAddressSync([Buffer.from('lvault'), pool.toBuffer()], PROGRAM);
const [pos1]  = PublicKey.findProgramAddressSync(
  [Buffer.from('lposition'), pool.toBuffer(), u64(1)], PROGRAM);
console.log(`pool_id ${poolId}\npool           ${pool.toBase58()}\nvault          ${vault.toBase58()}\n`);

writeFileSync('/tmp/locked_state.json', JSON.stringify({
  pool: pool.toBase58(), vault: vault.toBase58(),
  holderAsset: holderAsset.address.toBase58(),
  treasuryAsset: treasuryAsset.address.toBase58(),
  reserveAsset: reserveAsset.address.toBase58(),
  reserveOwner: Array.from(reserveOwner.secretKey),
}));

console.log('--- initialize_pool ---');
await send('initialize_pool', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: assetMint, isSigner: false, isWritable: false },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([
    disc('initialize_pool'), u64(poolId),
    payer.publicKey.toBuffer(),          // spv
    treasuryOwner.publicKey.toBuffer(),  // treasury
    reserveOwner.publicKey.toBuffer(),   // reserve
    u64(10_000_000),                     // min_investment
  ]),
}));

console.log('--- add_tier: 90 days, 8% APY, 2% exit penalty ---');
await send('add_tier', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
  ],
  data: Buffer.concat([
    disc('add_tier'),
    u32(90), u32(800), u32(200), u64(10_000_000), u8(1), Buffer.alloc(7),
  ]),
}));

console.log('--- set_open ---');
await send('set_open', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
  ],
  data: Buffer.concat([disc('set_open'), u8(1)]),
}));

console.log('--- deposit 2000.000000 into tier 0, interest upfront ---');
await send('deposit', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: pos1, isSigner: false, isWritable: true },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: holderAsset.address, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc('deposit'), u64(2_000_000_000), u32(0), u8(0)]),
}));

let v = await getAccount(conn, vault);
console.log(`\nvault after deposit   ${(Number(v.amount) / 1e6).toFixed(6)}`);
console.log(`  2000.000000 principal less 90d @ 8% interest paid upfront`);

console.log('\n--- exit_early ---');
const before = Number((await getAccount(conn, holderAsset.address)).amount);
await send('exit_early', new TransactionInstruction({
  programId: PROGRAM,
  keys: [
    { pubkey: pool, isSigner: false, isWritable: true },
    { pubkey: pos1, isSigner: false, isWritable: true },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: holderAsset.address, isSigner: false, isWritable: true },
    { pubkey: treasuryAsset.address, isSigner: false, isWritable: true },
    { pubkey: reserveAsset.address, isSigner: false, isWritable: true },
    { pubkey: reserveOwner.publicKey, isSigner: false, isWritable: false },
    { pubkey: reserveOwner.publicKey, isSigner: true, isWritable: false },
    { pubkey: payer.publicKey, isSigner: true, isWritable: false },
    { pubkey: payer.publicKey, isSigner: false, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ],
  data: disc('exit_early'),
}, [payer, reserveOwner]));

const after = Number((await getAccount(conn, holderAsset.address)).amount);
const tre = Number((await getAccount(conn, treasuryAsset.address)).amount);
v = await getAccount(conn, vault);
console.log(`\nholder received       ${((after - before) / 1e6).toFixed(6)}`);
console.log(`treasury penalty      ${(tre / 1e6).toFixed(6)}  (2% of 2000.000000 = 40.000000)`);
console.log(`vault left            ${(Number(v.amount) / 1e6).toFixed(6)}`);
console.log(`\nPOOL: ${pool.toBase58()}`);
