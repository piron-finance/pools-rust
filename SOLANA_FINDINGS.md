# Solana findings

Pre-production review of the three Anchor programs. Every instruction and every account
context was read, and each finding traced through its whole flow before being called a
defect.

The emphasis here differs from the other two chains, because Solana's failure modes do.
Most of what can go wrong is not in the handler logic but in the account context: the
caller supplies every account an instruction touches, so an account the program forgets to
constrain is an account the caller chooses. That is where the live finding below came from,
and where the earlier pass found the fee-destination defect too.

Reproductions are in each program's `tests/audit_probes.rs`, as regression tests asserting
the corrected behaviour.

---

## SOL-1 · High · The reserve's token account was unbound on both debt paths — **fixed**

**Where** `locked/src/lib.rs` — `ExitEarly` and `SettleDebt`

A locked pool borrows from the yield reserve when the vault cannot cover an early exit, and
the SPV repays that debt later. Neither account context tied the reserve's token account to
the reserve the pool records.

On the borrow path it carried no constraint at all:

```rust
#[account(mut)]
pub reserve_asset: Box<Account<'info, TokenAccount>>,
```

The handler does check `ctx.accounts.reserve.key() != ctx.accounts.pool.reserve`, but
`reserve` is a separate `UncheckedAccount` — nothing connected it to `reserve_asset`, which
is the account the tokens actually come from, authorised by a separate
`reserve_authority: Signer`. So the pool would book `reserve_debt += short` against the
reserve for money supplied from an entirely unrelated account.

The repay path is the sharper half:

```rust
#[account(mut, token::mint = pool.asset_mint)]
pub reserve_asset: Box<Account<'info, TokenAccount>>,
#[account(address = pool.spv)]
pub spv: Signer<'info>,
```

A mint constraint and nothing else. `settle_debt` transfers from `spv_asset` to
`reserve_asset` and then reduces `pool.reserve_debt` by the amount. **The SPV could pass a
token account it owned**, move money from one of its own accounts to another, and have the
pool record the reserve as repaid. The debt clears; the reserve receives nothing.

That is a single-role loss of real protocol capital, and the role is one the protocol
already trusts to hold drawn funds — so a compromised SPV key turns an accounting hole into
a withdrawal.

### Fix

Bound both to the reserve the pool records:

```rust
#[account(mut, token::mint = pool.asset_mint, token::authority = pool.reserve)]
pub reserve_asset: Box<Account<'info, TokenAccount>>,
```

Verified by `debt_can_only_be_repaid_to_the_reserve`: the SPV naming its own account is
refused, `reserve_debt` is unchanged by the attempt, and repaying the real reserve credits
it and clears the debt.

---

## SOL-2 · Medium · Integer overflow checks never reached these programs — **fixed**

**Where** `Cargo.toml:31-33` (workspace), and the three program manifests

The workspace sets the protection:

```toml
[profile.release]
overflow-checks = true
```

And then excludes the programs it would protect:

```toml
exclude = ["solana/single-asset", "solana/locked", "solana/stable-yield"]
```

None of the three defined a profile of its own. `cargo-build-sbf` builds in release, and a
release build turns overflow checks **off** by default — so every one of these programs was
compiled with silent wrapping arithmetic, while the rest of the repository was not.

The handlers are defensively written: balances go through `checked_add`, the `add()` helper,
or `saturating_sub`, and the plain `-=` sites are each preceded by a guard
(`if instrument.purchase_price > pool.in_flight { return Err(ExceedsInFlight) }` and
similar). I did not find a reachable wrap. That is the current state of the code, not a
property of it — the reason to compile with checks on is that the next unguarded
subtraction wraps a balance to `u64::MAX` instead of aborting the instruction.

### Fix

Added an explicit `[profile.release] overflow-checks = true` to each program manifest, with
a comment explaining why it cannot be inherited. All three rebuild and all 114 tests pass,
which also confirms nothing was relying on wrapping.

---

## SOL-3 · Low · Mint constraints were left to the token program — **fixed**

**Where** `stable-yield/src/lib.rs` — `Deposit`

```rust
#[account(mut, token::authority = holder)]
pub holder_asset: Box<Account<'info, TokenAccount>>,
#[account(mut, token::authority = holder)]
pub holder_shares: Box<Account<'info, TokenAccount>>,
```

Ownership was constrained; the mint was not. Both accounts are then used in CPIs —
a transfer out of `holder_asset` and a `mint_to` into `holder_shares` — and the SPL token
program rejects a mismatched mint on both, so a wrong-mint account cannot actually get
through.

It is not a live defect, but the guard sits one CPI downstream of the program that depends
on it. The same context in the other two programs constrains the mint locally
(`token::mint = pool.asset_mint`), so this was an inconsistency as much as a weakness.

### Fix

Added the mint constraints, so the context states its own requirements rather than
inheriting them from a callee.

---

## SOL-4 · High · Nothing deploys these programs, and nothing sets their upgrade authority — **open**

**Where** absent: there is no `scripts/deploy_solana.sh`

`scripts/deploy_soroban.sh` deploys the Stellar system end to end and, in its last step,
binds the timelock as upgrader on every contract. There is no equivalent for Solana.

On Solana a program's upgrade authority is not a contract the protocol writes — it is an
address held by the BPF loader, and whoever holds it can replace the whole program in one
transaction with no delay. `SOLANA_PLAN.md:192` already states the intended answer:

> **Upgrade authority: a Squads v4 multisig with a time-delay policy.**

Nothing executes it. A program deployed with `solana program deploy` keeps the deploying
keypair as its upgrade authority until somebody changes it, and there is no script, no
checklist and no test that does.

The asymmetry is worth stating plainly: EVM routes every upgrade through a 72-hour timelock
with a guardian brake, Stellar binds the same timelock on every singleton contract, and
Solana — as things stand — would ship three programs replaceable by one hot key.

### Fix — not applied in this pass

This is deployment tooling rather than program code, and it needs decisions I should not
make unilaterally: which Squads multisig, which signer set, what delay, and whether the
authority is eventually revoked outright once the programs settle. What I can say is that
it belongs on the critical path for the production push, alongside:

- `solana program set-upgrade-authority` to the multisig for all three programs, verified
  on chain afterwards rather than assumed
- the queue account's rent payer and size, since the stable-yield queue is a client-created
  22,600-byte account and whoever creates it funds it
- a recorded program id per cluster, the way `deployments/` records Stellar's

---

# Closed in the previous pass

Recorded here so this file is the whole Solana picture rather than half of it. Each was
reproduced before being fixed and carries a regression test.

| | Finding |
|---|---|
| Critical | **Coupon claimable twice by moving shares.** Entitlement read the live token balance; 200,000,000 distributed paid out 300,000,000. Now struck on a program-maintained `Position::shares`. |
| Critical | **Stable-yield NAV never accrued.** The binding never called the valuation engine, so a holding accreted invisibly and the price jumped at settlement — a depositor arriving on day 89 of 90 took half the term's yield. NAV is now computed from the instrument book, with a permissionless `refresh_nav_mirror` for readers. |
| Critical | **The caller named the account fees were paid into.** `treasury_asset` was constrained only by mint, so every withdrawal fee and early-exit penalty was optional. Now bound to `pool.treasury`. |
| Medium | **Transferred shares were unredeemable.** The holder record only `deposit` created, and creating it inside the failing instruction cannot work because the revert rolls it back. Hence `register_holder`. |
| Low | **Queue payout loop driven by payment count** rather than queue position. Latent; now structurally impossible. |

---

# Checked and found not to apply

Each is a confirmed finding on another chain. None is a defect here, and the reason in each
case is structural rather than luck.

### The cross-pool settlement defect (EVM S-1) cannot occur

On EVM, `positions` is one global mapping and the money-moving paths bound only to the
calling pool, so a position could be settled against another pool's escrow — proven at
50,616 and 45,185 drained.

Here a position is a PDA seeded by its pool:

```rust
seeds = [seeds::POSITION, pool.key().as_ref(), &position.position_id.to_le_bytes()]
```

and the pool itself is seeded by `pool_id`. A position address belongs to exactly one pool
by derivation. Passing another pool's position does not produce a wrong payout — it fails
to resolve the PDA at all.

### Retroactive penalty repricing (EVM S-3) cannot occur

`add_tier` appends and `set_tier_active` toggles one boolean. No instruction mutates a
tier's APY or penalty, so reading the penalty from the tier is safe here: the value cannot
change under an open position.

### Bare transfers to the reserve (EVM S-5, Stellar ST-1) have no analogue

There is no fee-manager program on Solana. Fees go to a treasury token account directly,
and the reserve is funded by explicit instructions that do their own accounting — no path
pushes tokens at a program that tracks a separate balance.

---

# Checked and found sound

- **Signer bindings.** `#[account(address = pool.authority)]` and
  `#[account(address = pool.spv)]` on every privileged context; the stable-yield handlers
  check the role explicitly and report 701, the number Soroban reports for the same
  condition, so the backend maps one code.
- **PDA derivation.** Canonical bumps throughout, either derived by Anchor or read from the
  account that stored it. `#[account(init, ...)]` prevents re-initialisation.
- **`remaining_accounts`.** `process_withdrawals` takes payee accounts from an operator, and
  checks each against the owner recorded on its own request before paying.
- **No arbitrary CPI.** `Program<'info, Token>` validates the program id, so a caller
  cannot substitute a look-alike token program.
- **Duplicate-account substitution.** The vault is a PDA owned by the pool, and holder
  accounts carry `token::authority = holder`, so a caller cannot pass the vault as their
  own account.
- **Build freshness.** The test harnesses refuse to run against a stale `.so`, because
  litesvm executes the compiled artefact rather than the crate `cargo test` just built. This
  fired correctly during the review when a `piron-core` change left the programs behind.

---

# Scope

Read in full: all three programs' `lib.rs`, `state.rs` and `seeds.rs`, every account
context, and the shared `piron-core` paths they call.

Not covered: compute-unit budgeting, and the devnet deployment itself — which is SOL-4.
