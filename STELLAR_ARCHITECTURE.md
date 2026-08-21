# Piron on Stellar — technical architecture & implementation plan

Status: **v1 in progress.** EVM (Solidity) and Stellar (Rust/Soroban) run side by side.
Solidity in `src/` is the reference implementation; Rust follows it.

Companion docs: [migration brief](https://claude.ai/code/artifact/0909f742-5900-474f-881e-facd92f2f23b) ·
[multi-chain architecture](https://claude.ai/code/artifact/c7c2e7ad-5070-4ddf-ac0b-9916bad0d03a) ·
conventions in `.claude/skills/piron-multichain/SKILL.md` · the Solana binding in
`SOLANA_ARCHITECTURE.md` · drift tracking in `parity/` · **the task queue in
`IMPLEMENTATION_PLAN.md`**.

---

## 1. The one structural decision

Everything else follows from this: **protocol logic and platform binding are separate crates.**

```
crates/
  piron-core/          no_std, zero dependencies, allocates nothing. No SDK, no Env,
                       no I/O. Math, state machines, validation. Shared with Solana.
soroban/
  piron-types/         Soroban wire types. No contract in it — see below.
  access-manager/  pool-registry/  fee-manager/  pool-escrow/  pool-factory/
  single-asset-manager/  locked-pool-manager/  stable-yield-manager/
solana/
  single-asset/        anchor-lang. Accounts, PDAs, signers, SPL.
```

`piron-types` exists because a factory needs a manager's constructor types to deploy it,
and depending on the manager crate for them drags that contract's exported symbols into
the factory's wasm, where they collide at link time. Shared `#[contracttype]` structs go in
a crate with no `#[contract]` in it.

Platform *policy* stays with its platform: TTL constants live in the Soroban binding, not
in core, because Solana has no TTL.

`piron-core` never learns what chain it is on. It receives balances, supply and the
current time as arguments and returns numbers and errors. The contract crate is the
only place that knows about `Env`, storage tiers, `require_auth`, or SEP-41 tokens.

Three things this buys, in order of how soon they pay off:

1. **Tests without a chain.** `cargo test -p piron-core` runs the whole protocol's
   arithmetic and state transitions in milliseconds with no host, no ledger, no fixtures.
2. **A cheap path to the shared spec.** When there is engineering capacity for
   conformance vectors, `piron-core` is already the thing they run against. Retrofitting
   this split later means rewriting every contract.
3. **A Solana port that is possible at all.** See §2.

The cost today is one extra crate boundary. That is the whole cost. This is the
"unless it's no work to do it today" case — it is close enough to no work that not
doing it would be the expensive choice.

---

## 2. Does the Rust work on Solana?

**Not the binding. Yes the core.** A `soroban-sdk` contract cannot compile for Solana:
different target (Wasm vs SBF), different SDK, different state model (contract key-value
with TTL vs program-owned accounts with rent-exemption), different auth, different token
standard.

`piron-core` ports unchanged, because it is `no_std`, has no dependencies, and receives
balances and time as arguments rather than fetching them. That is roughly **55–65% of the
Rust**, and it is the reason the Solana binding is a wiring project rather than a rewrite.

Full treatment in `SOLANA_ARCHITECTURE.md`, including the constraint that came back the
other way: Solana accounts are sized at creation, so the coupon schedule needed a bound,
so `piron_core::MAX_COUPON_SCHEDULE` is now a protocol rule on every runtime including
Stellar.

## 3. Module map

| Solidity | Rust | Crate |
|---|---|---|
| `libraries/CalculationLibrary.sol` | `single_asset::calc` | core |
| `libraries/ValidationLibrary.sol` | `single_asset::validate` | core |
| `libraries/PoolLifecycleLibrary.sol` | `single_asset::lifecycle` | core |
| `libraries/DepositWithdrawalLibrary.sol` | `single_asset::flow` | core |
| `libraries/LockedPool*.sol` | `locked::*` | core |
| `libraries/StableYield*.sol` | `stable_yield::*` | core |
| `types/IPoolTypes.sol` | `types` | core |
| `Manager.sol` | `soroban/single-asset-manager` | binding |
| `LiquidityPool.sol` (ERC-4626) | `single-asset-vault` (OZ `Vault`) | contract |
| `escrows/PoolEscrow.sol` | `single-asset-escrow` | contract |
| `AccessManager.sol` | OZ `stellar-access` RBAC + grant-delay wrapper | contract |
| `PoolRegistry.sol` | `registry` | contract |
| `FeeManager.sol` | `fees` | contract |
| `governance/TimelockController.sol` | OZ `TimelockController` | dependency |
| `governance/UpgradeGuardian.sol` | OZ `Pausable` + account thresholds | dependency + protocol |
| `factories/*.sol` | `Deployer` + salt | protocol |
| `UUPSUpgradeable` (14 files) | OZ `Upgradeable` trait | dependency |
| `ReentrancyGuard` (9 files) | — deleted, host forbids re-entry | — |

The eight libraries that exist to dodge EIP-170's 24KB limit collapse into modules. They
were a deployment workaround, not a design.

---

## 4. Storage design — Single Asset pool

One manager contract instance per pool (deployed by the factory with a salt), matching
the EVM model.

| Key | Tier | Contents |
|---|---|---|
| `Config` | Instance | `PoolConfig` — immutable after funding opens |
| `Data` | Instance | `PoolData` — status and running totals |
| `Registry`, `Escrow`, `Vault`, `Access` | Instance | Contract addresses |
| `User(Address)` | Persistent | `UserPoolData` — deposit time, coupons claimed |
| `Idem(BytesN<32>)` | Temporary | Idempotency keys for backend-submitted calls |

Instance storage is one entry, so extending its TTL extends config, data and all the
address pointers together — one bump per entry point covers the hot path.

Per-user entries are Persistent and individually bumped on touch. An investor who
deposits and then does nothing for a year still has a live entry only because the
keeper bumps it. **The keeper is part of the protocol, not ops garnish.**

Nothing iterates users. The Solidity does not either for Single Asset; where the other
pool types do (`queue.head..queue.tail`, `positionIds`), those become cursor-batched
entry points that take an explicit range and a maximum count, sized to the 100-entry
footprint cap.

---

## 5. Numbers, time, decimals

- **Amounts are `i128`.** Products must stay under ~1.7e38. `mul_div(shares, value, supply)`
  with both operands at USDC scale (7 decimals) overflows only above ~$1.3 trillion in a
  single pool. Overflow returns `Error::Overflow` rather than wrapping or panicking. If a
  pool ever needs more headroom, that is a 256-bit `mul_div`, not a type change.
- **Basis points are `u32`** out of `BASIS_POINTS = 10_000`, same as Solidity.
- **Time is `u64` unix seconds** in `piron-core`. Ledger sequence numbers appear only in
  the contract layer, and only for TTL. Do not mix the two units.
- **Decimals differ across chains.** USDC is 6 on EVM, 7 as a Stellar classic asset. The
  contract layer reads `decimals()` from the token and passes scaled integers into core;
  core never assumes a scale. The backend's `Decimal(18,6)` columns need widening before
  Stellar data lands in them — see the architecture brief.

---

## 6. Parity policy for v1

**Solidity is the reference. Rust follows.** No shared specification yet; the two are kept
honest by three cheap disciplines instead:

1. `// PARITY:` comments on every intentional divergence (enforced by review).
2. `piron-core` unit tests ported from the Foundry tests case by case, keeping the same
   scenario names so a reviewer can diff them by eye.
3. Stable numeric error codes shared by both implementations, so the backend maps one
   taxonomy.

When there is capacity, the upgrade path is already cleared: extract scenarios into JSON
vectors, run them from Foundry and from `cargo test`, and make CI fail on divergence. That
is a harness, not a rewrite, precisely because `piron-core` is pure.

---

## 7. Implementation phases

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, `piron-core`, math, types, Single Asset calc + validation | **written — 45 tests** |
| 1 | Single Asset lifecycle in core (funding → invested → matured → withdrawn) | **written — `lifecycle.rs` + `flow.rs`, 34 tests** |
| 2 | `single-asset-manager` contract: storage, TTL, auth, events | **scaffold** — patterns established |
| 3 | Vault (OZ `Vault`) + escrow contracts, factory with salt | |
| 4 | Registry, access control with grant delay, fee manager | |
| 5 | Locked pools in core + contracts | |
| 6 | Stable yield: NAV, cursor-batched withdrawal queue | |
| 7 | TTL keeper service, backend `StellarAdapter`, indexer | |
| 8 | Audit, testnet, mainnet | |

Phases 0–2 are the ones that prove the architecture. Everything after is repetition of an
established pattern.

**What exists right now** (`rust/`, ~1,700 lines):

- `piron-core` — `error` (stable codes), `math` (checked `mul_div`, bps, pro-rata),
  `types` (from `IPoolTypes.sol`), `single_asset::calc` (from `CalculationLibrary.sol`),
  `single_asset::validate` (from `ValidationLibrary.sol`, plus `validate_config`),
  `single_asset::lifecycle` (from `PoolLifecycleLibrary.sol`), `single_asset::flow` (from
  `DepositWithdrawalLibrary.sol`). 79 tests, scenario names mirroring the Foundry suite.
- `soroban/single-asset-manager` — storage keys and tiers, TTL policy, the host↔core conversion
  boundary, TTL bump helpers, and `record_deposit` / `record_coupon` / `claim_coupon` /
  `pool_value` demonstrating the auth → bump → delegate → emit pattern. Held out of the
  workspace until `soroban-sdk` is pinned to a confirmed release.

**Nothing has been compiled.** The machine this was written on has no Rust toolchain, so
the first `cargo test -p piron-core` is real Phase 0 work, not a formality.

---

## 8. Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32v1-none
cargo install --locked stellar-cli

cd rust
cargo test -p piron-core                       # logic, no chain needed
cd soroban/single-asset-manager
cargo build --target wasm32v1-none --release   # once soroban-sdk is pinned
```

Stellar publishes agent skills and an MCP server (`skills.stellar.org`) worth installing
alongside this repo's own skill.
