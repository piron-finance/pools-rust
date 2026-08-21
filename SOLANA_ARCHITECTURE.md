# Piron on Solana — technical architecture & implementation plan

Status: **scaffold.** Solidity in `src/` is the reference; `piron-core` holds the logic;
this document covers the Solana binding only.

Companions: `STELLAR_ARCHITECTURE.md` (the other binding) ·
`.claude/skills/piron-multichain/SKILL.md` (conventions) · `parity/` (drift tracking) ·
**`IMPLEMENTATION_PLAN.md` (the task queue)**.

---

## 1. What is shared, and what is not

`rust/crates/piron-core` is the whole protocol: the arithmetic, the state machine, the
validation. It is `no_std` with zero dependencies and takes balances, supply and time as
arguments. Solana and Stellar both link it and neither can tell which one is running.

Everything below the logic is different, and the differences are not cosmetic:

| | Soroban | Solana |
|---|---|---|
| Compile target | Wasm (`wasm32v1-none`) | SBF, via `cargo-build-sbf` |
| Framework | `soroban-sdk` | Anchor (or Pinocchio) |
| State location | Key-value inside the contract | Separate accounts owned by the program |
| Addressing | Storage key | PDA derived from seeds |
| State lifetime | TTL + rent; archives if it lapses | Rent-exempt minimum; permanent |
| Keeper needed | **Yes** | **No** |
| Size limits | Entry-size caps, grows freely | Fixed at creation; +10KB per realloc, 10MB max |
| Declared access | Ledger footprint | Account list per instruction |
| Auth | `require_auth()`, host-verified | `Signer`, framework-verified |
| Tokens | SEP-41 / Stellar Asset Contract | SPL Token / Token-2022 |
| Compliance | OpenZeppelin RWA (ERC-3643) | Token-2022 extensions |
| Money type | `i128` | `u64` |
| USDC decimals | 7 | 6 |
| Re-entrancy | Forbidden by host | Reentrant CPI forbidden |

Two of those deserve emphasis because they invert a habit:

- **No TTL means no keeper.** The scheduled job that keeps Stellar positions out of the
  archive has no Solana counterpart, and building one would be cargo-culting. Fund an
  account to the rent-exempt minimum and it persists.
- **Accounts are sized at creation.** Nothing grows implicitly. This is why
  `piron_core::MAX_COUPON_SCHEDULE` exists — a constraint discovered on Solana and
  promoted to a protocol rule everywhere, so a pool valid on one runtime is valid on all.

---

## 2. Account design — Single Asset pool

| Account | Seeds | Contents | Lifetime |
|---|---|---|---|
| `Pool` | `[b"pool", pool_id]` | Terms and running state | Rent-exempt, permanent |
| `Position` | `[b"position", pool, owner]` | Per-investor deposit time and coupons claimed | Rent-exempt, created on first deposit |
| Vault token account | `[b"vault", pool]` | Pool's asset balance, program-owned | Rent-exempt |

Seeds are protocol surface. Changing one changes every derived address, so they are
append-only in the same way error codes are.

`Position` is created on first deposit with the investor as payer, which makes onboarding
cost a few thousand lamports. That is the Solana analogue of Stellar's trustline step, and
like the trustline it can be sponsored — the program can pay from a treasury PDA if the
product wants a zero-friction first deposit.

Every account carries an explicit `LEN` const. `Pool::LEN` includes
`8 * MAX_COUPON_SCHEDULE + 4 * MAX_COUPON_SCHEDULE` for the fixed schedule arrays; that is
the visible price of the bound, and it is worth it.

---

## 3. Compliance

Stellar gets ERC-3643 from OpenZeppelin's RWA module — identity registry, claim issuers,
compliance hooks, freezing, forced transfer, recovery — as a library.

Solana's equivalent is assembled from Token-2022 extensions rather than imported:

| Need | Extension |
|---|---|
| Transfer eligibility / allowlist | **Transfer Hook** — a program invoked on every transfer |
| Forced transfer, recovery | **Permanent Delegate** |
| Locked until KYC clears | **Default Account State** (frozen by default) |
| Freezing a holder | Freeze authority |
| On-chain terms | Metadata extension |

This is the same compliance stack Paxos and institutional RWA issuers run on Solana, so it
is well-trodden. Two cautions: the hook program is ours to write and audit, where the
Stellar side is auditing someone else's library; and Permanent Delegate plus Transfer Hook
have both been the subject of 2026 attack research, so they need real scrutiny rather than
being switched on because the docs list them.

---

## 4. Numbers, time, decimals

- **Amounts are `u64` on-chain.** Core works in `i128`. Widening in is free; narrowing on
  the way out is `try_from` and never `as`. A value that does not fit a `u64` is a bug, not
  a rounding artifact.
- **`Clock::get()?.unix_timestamp` is `i64`.** Clamped to `u64` at the boundary. Core takes
  unix seconds on every runtime; slot numbers never enter core.
- **USDC is 6 decimals on Solana**, the same as EVM — the seven-decimal problem is
  Stellar-specific. The backend still needs one `Amount` type that carries its own scale,
  because it will see both.
- **Compute budget.** The default per-instruction allowance is modest and raisable with a
  compute-budget instruction. Batch operations — the withdrawal queues in the other pool
  types — must be cursor-based with a bounded account list, for the same reason they must
  be on Soroban and for a different mechanical reason. Confirm current limits before
  sizing; the network's block limit has been moving upward through 2026.

---

## 5. Framework choice

**Anchor for v1.** Readable account constraints, IDL generation for the backend and
frontends, and the ecosystem an auditor expects. The cost is binary size and compute.

**Pinocchio if either becomes a problem.** It is `no_std`, dispenses with Anchor's
deserialization, and has produced order-of-magnitude binary reductions and materially lower
compute on equivalent programs. Because the logic already lives in a `no_std` core, moving
a binding from Anchor to Pinocchio is a rewrite of the wiring only — which is exactly the
kind of decision the core/binding split is meant to keep cheap.

One Anchor detail that is not optional: **do not use `#[error_code]`.** Anchor offsets
custom errors by 6000, which would make the same protocol condition report a different
number on Solana than on Stellar and break the backend's single error taxonomy. Return
`ProgramError::Custom(err.code())` with the core code verbatim.

---

## 6. Module map

| Solidity | `piron-core` | Solana |
|---|---|---|
| `libraries/CalculationLibrary.sol` | `single_asset::calc` | — (delegated) |
| `libraries/ValidationLibrary.sol` | `single_asset::validate` | — (delegated) |
| `types/IPoolTypes.sol` | `types` | `state::Pool` (account layout) |
| `Manager.sol` | — | `piron_single_asset` program |
| `LiquidityPool.sol` (ERC-4626) | — | SPL mint + vault PDA |
| `escrows/PoolEscrow.sol` | — | program-owned token account |
| `AccessManager.sol` | — | access program + role PDAs |
| `factories/*.sol` | — | PDA derivation, no factory needed |
| `UUPSUpgradeable` | — | BPF upgradeable loader + upgrade authority |
| `ReentrancyGuard` | — | not needed; reentrant CPI is forbidden |

Two rows worth noting: **factories disappear** — a PDA is deterministic by construction, so
there is nothing to deploy. And upgrades are a loader-level concern with an upgrade
authority, which should be the same multisig that holds the Stellar and EVM upgrade rights.

---

## 7. Phases

| Phase | Scope | Status |
|---|---|---|
| 0 | `piron-core` (shared with Stellar) | Single Asset complete, 79 tests, not yet compiled |
| 1 | Program scaffold: accounts, PDAs, three entry points | **scaffold written** |
| 2 | Anchor pinned, builds, `cargo-build-sbf` green, localnet tests | next |
| 3 | SPL mint + vault PDA, deposit and redemption flows end to end | |
| 4 | Token-2022 compliance stack: transfer hook, permanent delegate, default state | |
| 5 | Access control program, fee accounts | |
| 6 | Locked pools, then stable yield | |
| 7 | Backend `SolanaAdapter`, indexer, wallet layer | |
| 8 | Audit, devnet, mainnet-beta | |

Solana lags Stellar by design: Stellar has the funding path and the more novel
constraints. Solana reuses whatever Stellar's phases prove about the core.

---

## 8. Toolchain

```bash
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
cargo install --git https://github.com/coral-xyz/anchor avm --locked
avm install latest && avm use latest

cd rust
cargo test -p piron-core          # shared logic, no chain
cd solana/single-asset && cargo build-sbf
```

`rust/solana/single-asset` is excluded from the Cargo workspace until `anchor-lang` is
pinned to a confirmed release — `cargo test -p piron-core` needs no network and no Solana
toolchain.
