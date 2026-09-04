# Solana — implementation plan

Written after Stellar shipped, so the leverage is known: `piron-core` is settled and
proven on chain. Solana is binding work, not protocol work. Nothing below re-derives a
formula.

## What will actually bite

Ordered by how much time each costs if it surprises you mid-implementation rather than
being decided up front.

### 1. The amount type boundary

`piron_core::Amount` is `i128`. SPL token amounts are `u64`. Widening on the way in is
free; narrowing on the way back is not, and `as u64` wraps silently.

Every value returned by core and written to an account crosses this boundary. There is no
helper for it yet, and a scaffold that already does `self.target_raise as i128` in one
direction will invite `outcome.shares as u64` in the other.

**Decide first:** one checked conversion in `state.rs`, used everywhere, that errors rather
than truncates. Write it before the first instruction, not after the first bug.

### 2. One program, many pools

Soroban deploys a contract instance per pool, so `stable-yield-manager` holds one pool's
state and every accessor is implicit. Solana deploys the program once and derives a PDA per
pool, so every instruction takes the pool account explicitly and must verify its seeds.

Consequences that are easy to miss:

- `pool-registry` and `pool-factory` largely dissolve. Creating a pool is `init` on a PDA,
  and the registry is a `getProgramAccounts` filter. Do not port those two contracts.
- Every handler needs the seed check. Anchor's `seeds`/`bump` constraints do it
  declaratively; anything hand-rolled is where authority bugs live.
- `upgrade-timelock` maps to the BPF upgrade authority, which is a different mechanism —
  a multisig or a governance program holding the authority, not a contract we write.

### 3. The withdrawal queue

The one place a direct port does not work. Soroban grows a queue in contract storage
freely; a Solana account is sized at creation and reallocs 10KB at a time.

Options, in preference order:

1. **A ring buffer of fixed capacity in the pool account**, with head/tail cursors — the
   same shape `piron-core`'s queue already uses. Capacity becomes a config decision.
2. One PDA per withdrawal request, keyed by index. Unbounded, but processing a batch means
   passing N accounts and the transaction account limit binds.

`piron-core::stable_yield::queue` is already cursor-based, which was the right call. Confirm
it can drive option 1 unchanged before writing any account layout.

### 4. Compute units

`process_withdrawals` and any batch maturity loop run inside a 200k CU budget (1.4M with a
request). Soroban's limits are generous by comparison and the current code was not written
against a CU ceiling.

Every batch entry point needs an explicit bound and a test that runs it at that bound.

### 5. Rent and account closure

Redeemed positions should be closed and their rent returned to the holder. Anchor's `close`
constraint does this, but only if it is on the account — forgetting it means every position
ever opened is a permanent lamport cost to somebody.

### 6. Toolchain, from zero — settled

Solana CLI 3.1.10, `cargo-build-sbf` 3.1.10, Anchor 1.1.2. The scaffold's `TODO(pin)` guessed
`0.31`; Anchor has since cut a 1.0, so the guess was a major version out and would have
failed at the first build.

Two things the guess also hid. The crate sits outside the workspace, so it cannot inherit
`[workspace.package]` — version, edition and license are spelled out in its own manifest and
have to be kept in step. And `init_if_needed` is feature-gated: a position account is created
on a holder's first deposit, and Anchor refuses the pattern until you acknowledge the
re-initialisation risk. It is safe here because the position is a PDA seeded on (pool, owner),
so a second init resolves to the same address.

### What is not a risk

Token-2022 compliance — transfer hook, permanent delegate, default account state — is
described in `SOLANA_ARCHITECTURE.md` §3 as the ERC-3643 analogue. **Stellar does not
implement on-chain compliance either**, and KYC lives in the backend and frontend. That
section describes a road not taken on either chain. Skip it, and correct the doc.

This removes the largest Solana-specific unknown: writing and auditing our own transfer
hook program.

## What the product actually needs from a chain

Read out of `pironBackend` and the three apps, because the off-chain system is what the
program has to serve, and two of its assumptions do not survive the move.

| | Count | Shape on EVM |
|---|---|---|
| Distinct on-chain reads | 58 | `calculatePoolNAV()`, `getNAVPerShare()`, `getPoolTiers()` |
| Unsigned transactions built server-side | 43 | backend returns `{to, data}`, a wallet signs |
| Events indexed | 2 | `Deposit`, `PoolWithdrawal`, filtered by topic and pool address |
| Source of truth for the UI | Postgres | `PoolPosition` mirrored from events; the chain is read only for live NAV |

That last row is the one to hold on to. The product reads Postgres, not the chain. The chain
settles and emits; anything the indexer cannot see, the product cannot show.

### There are no view functions

All 58 reads become fetch-account-and-deserialize. `calculatePoolNAV` cannot be a call, so
either the client recomputes NAV from the pool and instrument accounts, or the program
writes NAV to an account on every mutation.

Write it. A client that recomputes will eventually disagree with what the program believed
at the moment it moved money, and that disagreement surfaces as a support ticket about a
share price. Soroban already emits NAV on every mutation for the same reason.

### There are no indexed logs

`getLogs({topics, address})` has no equivalent. Anchor's `emit!` writes base64 into
unstructured program logs, found by walking `getSignaturesForAddress` and parsing
transactions.

So `IndexerState.lastBlock` becomes a **signature cursor**, not a slot — slots skip, and
signatures are the only stable pagination. This is the largest change to off-chain code and
the reason event design comes before the instruction set rather than after it.

### Every transaction carries its own account list

`{to, data}` is self-contained. A Solana instruction needs every account it touches, each
flagged signer or writable, plus a blockhash and a compute budget. The 43 builders get
materially heavier, and the PDA seed scheme becomes a backend concern.

Publish the seeds as a shared constant. Deriving them by hand in Rust and again in
TypeScript is a class of bug that only shows up against a real cluster.

### The approve-then-call two-step collapses

`requiresApproval` exists in three SPV flows because ERC20 needs an allowance first. On
Solana the owner signs one transaction that moves the tokens. Design for the single
transaction rather than porting the two-step.

### Shares stay a plain SPL mint

The app reads the asset balance directly and takes shares from the backend mirror, and
nothing in the UI transfers shares — so a program-internal balance would work. A plain mint
with the pool PDA as authority costs nothing, gives wallet visibility and composability, and
`transferPosition` on locked pools shows transferability is already in the model.

---

## Sequence

Each step ends in something runnable. Event and seed design come before the instruction set
because both are hard to change once instructions depend on them.

| | Step | Done when |
|---|---|---|
| 1 | ~~Toolchain: Solana CLI, Anchor, `cargo-build-sbf`; pin the real Anchor version~~ **done** | `cargo build-sbf` succeeds on the existing scaffold |
| 2 | The checked `i128`↔`u64` boundary, plus `Pool::LEN` and `Position::LEN` verified against `size_of` | A test asserts both sizes and a conversion test covers overflow |
| 3 | PDA seeds as a published constant, and the event set the indexer needs | A TypeScript consumer derives the same addresses from the same constants |
| 4 | NAV written to the pool account on every mutation | Reading the account gives the figure the program acted on |
| 5 | Single-asset instruction set, mirroring the Soroban entry points | The two `lib.rs` files read side by side as the same sequence |
| 6 | SPL mint for shares, vault PDA, escrow authority | Localnet: deposit mints, withdraw burns, vault balances move |
| 7 | Localnet integration: the full arc, including the shortfall case | Escrow lands on zero with nothing outstanding, as on Stellar |
| 8 | Locked pools, with the reserve-backed early exit | Same arc, per-position payouts |
| 9 | Stable yield, queue last | The ring buffer under a compute-unit bound |
| 10 | Devnet, and the same on-chain verification Stellar got | Shortfall case and emergency guard confirmed on chain |

## Decisions taken

**Withdrawal queue: capacity 256, in its own account.**

A realistic worst case is around 110 concurrent pending withdrawals on one pool, which
lands awkwardly close to a hard limit: an account created through a CPI can be at most
10,240 bytes, and at 81 bytes per request that is 110 entries almost exactly. Sizing the
buffer at the expected worst case leaves nothing, and a full queue means withdrawal
requests revert.

256 needs one `init` plus two `realloc`s at pool setup — two extra instructions, once —
for roughly 0.29 SOL of rent-exemption, recoverable when the pool closes. Keeping the
queue out of the pool account also stops every NAV read loading 20KB it does not need.

This is the first place the port genuinely diverges rather than being wired differently:
neither Stellar nor EVM has a per-account size ceiling.

**Upgrade authority: a Squads v4 multisig with a time-delay policy.**

There is no timelock contract to deploy — on Solana the program's upgrade authority is
just an address, and best practice is a multisig rather than a keypair. Squads v4 is the
de facto standard and supports time-locked execution, so the 72-hour delay
`upgrade-timelock` enforces on Stellar is a policy setting rather than a program we write
and audit.

Making the program immutable by setting the authority to `None` is a decision for after an
audit, not before one: it also removes the ability to fix what an audit finds.

**Share token: a plain SPL mint owned by the pool PDA.**

Token-2022's extensions — transfer hook, permanent delegate, default-frozen accounts — are
the ERC-3643 analogue, and KYC lives in the backend and frontend on every chain. The hook
would be a program we write and audit ourselves, where the Stellar equivalent would have
been someone else's library, and Token-2022 mints still have uneven wallet and DEX support.

Nothing downstream needs them.
