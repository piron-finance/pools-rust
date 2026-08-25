# Piron multi-runtime — implementation plan

**This file is the handoff.** Anyone picking the work up — human or agent — should be able
to read it, run two commands, and know exactly what to do next without re-deriving the
architecture.

Read first: `.claude/skills/piron-multichain/SKILL.md` (conventions, non-negotiable),
then `STELLAR_ARCHITECTURE.md` or `SOLANA_ARCHITECTURE.md` for whichever runtime you are
touching.

```bash
cd rust && cargo test -p piron-core   # is the logic green?
python3 parity/check.py               # has the Solidity moved under us?
```

---

## Ground rules

1. **`src/` is the reference and is off limits.** Never edit Solidity to make a port
   easier. If the port exposes a real bug, record it under *Open questions* below and
   raise it as its own change.
2. **Protocol decisions go in `piron-core`; nothing else does.** If you are writing an
   `if` that a chain could disagree with, it belongs in core.
3. **Every task below is finished only when its acceptance criteria pass**, not when the
   code is written.
4. **Re-baseline after reflecting Solidity changes**: `python3 parity/check.py --accept`.

---

## Where things stand

| | State |
|---|---|
| Repo | This one. The Solidity reference is a separate repository; `make parity` reads it and never writes to it. |
| `piron-core` | All three pool types, governance, upgrades, reserve. `no_std`, zero dependencies, allocation-free. |
| `access-manager` | Seven roles, 24h grant timelock, multisig execution, pause. |
| `upgrade-timelock` | 72h upgrade delay, 7-day window, guardian brake that lapses after 30 days. |
| `pool-registry` | Pools, approved assets, vetted SPVs. |
| `fee-manager` | Per-type splits, per-asset accounting, distribution. |
| `yield-reserve` | Loan facility, shortfall cover, treasury sweeps, floor discipline. |
| `pool-escrow` | Segregated custody, manager-only releases. |
| `pool-share` | SEP-41 share token. Positions are transferable. |
| `pool-factory` | Deterministic deployment of manager, escrow and share token together. |
| `single-asset-manager` · `locked-pool-manager` · `stable-yield-manager` | The three pool types. |
| `solana/single-asset` | Binding scaffold against the same core. Excluded pending an Anchor pin. |
| Deployment | **Live on testnet.** `make deploy`. |

**348 tests, 11 contracts, zero warnings.**

Stellar is feature-complete against the Solidity: every component in the reference
architecture has a counterpart, including the ones that were missing a cycle ago —
upgradeability behind a timelock, the yield reserve, and transferable shares.


**276 tests, 8 contracts, zero warnings.**


**271 tests passing, 7 contracts.** `make soroban-test`, `make soroban-deploy`.


**214 tests passing.** `make soroban-test`.


Two design changes the compiler forced, both improvements:

- **`piron-core` allocates nothing.** A `no_std` Soroban contract has no heap allocator, so
  the `Vec<u64>` coupon schedule failed the wasm link. Replaced by
  `schedule::CouponSchedule`, a fixed-capacity inline type bounded by `MAX_COUPON_SCHEDULE`
  — which already existed for Solana's fixed-size accounts. One constraint, two runtimes,
  and the dates/rates pairing invariant now lives in the type instead of being re-checked
  at each use.
- **Share balances live in the manager** until the vault contract exists (S5), rather than
  being passed in as arguments. The scaffold's `TODO(vault)` shortcut made the contract
  tests meaningless; holding the balances makes the whole lifecycle testable.

Ported Solidity, and where it went:

| Solidity | Rust |
|---|---|
| `types/IPoolTypes.sol` | `types.rs` |
| `libraries/CalculationLibrary.sol` | `single_asset/calc.rs` |
| `libraries/ValidationLibrary.sol` | `single_asset/validate.rs` + `flow.rs` |
| `libraries/PoolLifecycleLibrary.sol` | `single_asset/lifecycle.rs` |
| `libraries/DepositWithdrawalLibrary.sol` | `single_asset/flow.rs` |

---

## Defects in the reference — all closed

Five findings surfaced by re-deriving the protocol against a second runtime, plus more
found while hardening. Three were in the redemption path and shared one root cause: **a
payout computed from state that paying it out does not change.**

All are fixed in both runtimes. The record, including what was withdrawn and what remains
open, is in `AUDIT_RECORD.md`.

The approach is worth keeping: fix the Solidity first, then let the failing `piron-core`
tests prove the correction reached both runtimes. Those failures are the point — a test
that encodes the old behaviour and does not break is a test that was not testing anything.

## Stellar — the queue

### ~~S1. Compile the core~~ · **done**
84 tests green. `cd rust && cargo test -p piron-core`.

### ~~S2. Pin `soroban-sdk` and build the manager~~ · **done**
Pinned at 27.0.6, in the workspace, builds to a 55KB wasm.
**Still outstanding:** confirm the TTL constants in `src/ttl.rs` are at or below the
network maximum — `stellar network settings`. They are set to 60 days on the assumption
that matches Spiko's reported ceiling; that has not been verified against live settings.

### ~~S3. Complete the manager's entry points~~ · **done**
All present: `__constructor`, `deposit`, `mark_filled`, `close_epoch`,
`force_close_epoch`, `withdraw_for_investment`, `confirm_investment`, `record_coupon`,
`distribute_coupons`, `claim_coupon`, `settle_maturity`, `withdraw`, `mark_withdrawn`,
`declare_emergency`, plus views. Roles are held in instance storage as a placeholder for
the full `AccessManager` (S7). Original mapping, for reference:

| Entry point | Core function | Caller |
|---|---|---|
| `initialize` | `validate::validate_config` | factory |
| `mark_filled` | `lifecycle::mark_filled` | anyone |
| `close_epoch` | `lifecycle::close_epoch(force=false)` | operator |
| `force_close_epoch` | `lifecycle::close_epoch(force=true)` | admin |
| `withdraw_for_investment` | `lifecycle::validate_spv_withdrawal` | spv |
| `confirm_investment` | `lifecycle::process_investment` | spv |
| `settle_maturity` | `lifecycle::process_maturity` | spv |
| `mark_withdrawn` | `lifecycle::mark_withdrawn` | operator |
| `declare_emergency` | `lifecycle::declare_emergency` | emergency role |
| `withdraw` | `flow::withdraw` | holder |

### ~~S4. Contract tests~~ · **done**
20 tests. Full arc fund → fill → close → invest → mature → redeem; the underfunded path to
`EMERGENCY` with partial refunds; coupon receive → distribute → claim → re-claim rejected;
role enforcement per entry point; one test with no `mock_all_auths` proving `require_auth`
actually bites; one that advances the ledger past a TTL threshold and reads the position
back.

### ~~S6. Escrow contract~~ · **done**
`rust/soroban/pool-escrow`. Holds a SEP-41 asset; `take` and `release` are manager-only;
the manager is bound once and cannot be re-pointed. Every manager money path now moves
real tokens: deposits into custody, SPV draws, coupon receipts and claims, redemptions,
and the fee to the treasury.

### S5. Vault contract · **next, and no longer urgent**
Share balances live in the manager, which made the whole lifecycle testable without a
vault. Splitting them into an OpenZeppelin `Vault` gives holders a transferable SEP-41
share token — which is a product feature (secondary transfer of positions), not a
correctness fix.
**Done when:** shares are a SEP-41 token; the manager reads balance and supply by
cross-contract call; `shares_of` / `total_supply` on the manager become thin forwards.

### ~~S6b. Deploy to testnet~~ · **done**
Re-verified after the repo split and the factory: a pool created through the factory took
a 100 XLM deposit into real custody, and executing a role grant before its 24 hours were
up was refused with error 602.
`scripts/deploy_soroban.sh testnet` deploys all five contracts in dependency order, wires
them, and writes `deployments/soroban-testnet.env`. A 100 XLM deposit has been executed
against the live pool: escrow custody confirmed, shares minted 1:1, events emitted, and a
sub-minimum deposit refused with error 202 — a `piron-core` code surfacing from a deployed
Stellar contract, which is the shared taxonomy proving itself end to end.

### ~~S7. Registry and access control~~ · **done**
Seven roles, the 24-hour grant timelock with multisig execution, immediate multisig-only
revocation, `renounce_role` that always fails by design, and a pause that emergency can
set but only admin can clear. The registry gates pools on approved assets and vetted SPVs.

### ~~The factory~~ · **done**
`pool-factory` deploys a manager and its escrow from one salt, binds them, and registers
the pool — atomically, so there is no window where an escrow exists unbound or a manager
points at one that will take orders from someone else. Addresses are knowable before the
transaction: on the live testnet deployment the predicted pair matched the created pair
exactly.

### ~~S8. Fee manager~~ · **done**
Per-fee-type splits validated to total exactly 100%, split at collection so a policy change
never re-cuts money already taken, ops absorbing truncation so the parts always sum back to
the whole.

### ~~Upgradeability~~ · **done**
Every contract names the timelock as its upgrader, once and irrevocably. Nothing in the
system can have its bytecode replaced without 72 hours' notice, a 7-day execution window,
multisig execution by a different hand than scheduled it, and a guardian able to stop it —
for at most 30 days, after which the brake lapses so a lost key cannot freeze governance.

### ~~Yield reserve~~ · **done**
`yield-reserve`. Lends to a pool that owes a redemption before its capital is back, absorbs
a shortfall so it lands on the protocol rather than the last investor out, and sweeps real
surplus to the treasury. Lending and sweeping stop at the floor; covering a loss may go
below it, deliberately.

### ~~Transferable shares~~ · **done**
`pool-share`, a SEP-41 token per pool. Only the manager mints or burns; holders transfer,
approve and delegate. Verified on testnet by moving half a live position to another
account and having the manager agree about the new balance.

### S9. TTL keeper
S9 is a backend job, not a contract: it extends TTLs on positions no user has touched, and
alerts on headroom through the existing watchtower module. Design it against
`rust/soroban/single-asset-manager/src/ttl.rs`.

---

## Solana — the queue

Deliberately behind Stellar. Everything Stellar proves about `piron-core` is reuse here.

### L1. Pin Anchor and build
`rust/solana/single-asset/Cargo.toml` carries `TODO(pin)`.
**Done when:** `cargo build-sbf` succeeds; crate moved into workspace `members`.

### L2. Account sizing and init
`Pool::LEN` and `Position::LEN` are hand-computed and unverified.
**Done when:** a localnet test creates both accounts at exactly `LEN` bytes and the
rent-exempt lamports are asserted; a `#[test]` cross-checks `LEN` against
`std::mem::size_of` plus the discriminator.

### L3. Mirror S3 — complete the instruction set
Same table as S3, same core functions. Different wiring only.
**Done when:** the instruction set matches the Soroban entry points one for one, and a
reviewer can read the two `lib.rs` files side by side and see the same sequence.

### L4. SPL mint and vault PDA · L5. Token-2022 compliance · L6. Access program
L5 is the ERC-3643 analogue: transfer hook, permanent delegate, default account state.
Note the 2026 attack research on the first two — this needs a real threat model, not a
feature checklist.

### L7. Localnet integration tests · L8. Devnet

---

## Cross-cutting

- **CH1. Backend `ChainAdapter` port.** Eight domain services import `ethers` directly
  today. Unrelated to Rust; do it whenever.
- **CH2. Conformance vectors.** Only worth building when a second engineer joins. The
  design is settled: express `tests/` scenarios as JSON, run them from Foundry and from
  `cargo test`, index them off `parity/manifest.json`.
- **CH3. Locked pools, then stable yield.** Same shape as Single Asset: port the libraries
  into `piron-core` first, with tests, before touching either binding. Stable yield's
  withdrawal queue is the one place where both runtimes need cursor-batching rather than a
  direct port.

---

## Definition of done, any task

- [ ] `cargo test -p piron-core` green
- [ ] `python3 parity/check.py` clean
- [ ] Divergences from the Solidity carry `// PARITY:` comments
- [ ] New entry points: signer checked; Soroban ones extend instance TTL
- [ ] No `unwrap`/`expect`/`panic!` on a user-reachable path; no `as` on a narrowing cast
- [ ] Error codes appended, never renumbered
- [ ] `src/` untouched
