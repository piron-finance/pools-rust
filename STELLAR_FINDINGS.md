# Stellar (Soroban) findings

Pre-production review of the Soroban contracts. Every contract under `soroban/` was read,
and each finding was traced through its whole flow before being called a defect.

Several EVM findings have **no analogue here**, and the reasons are architectural rather
than incidental. Those are recorded at the end, because "we checked and it does not apply"
is a result, and assuming otherwise is how a review invents work.

Reproductions are in each contract's own `src/test.rs`, as regression tests asserting the
corrected behaviour.

---

## ST-1 · High · Fee revenue reached the reserve but could never be spent — **fixed**

**Where** `fee-manager/src/lib.rs:318`, `yield-reserve/src/lib.rs`

`fee-manager::distribute` pays each destination with a plain token transfer:

```rust
if acc.pending_reserve > 0 {
    client.transfer(&here, &destinations.reserve, &acc.pending_reserve);
}
```

The reserve governs every outflow against `stats.balance` — `available_for_loan`,
`validate_loan`, `validate_shortfall_cover`, `available_to_sweep` all read it. A pushed
transfer does not touch it.

The only crediting inflow is `receive_yield`, and it **pulls**:

```rust
Self::require_source(&env, &from)?;
client.transfer(&from, &env.current_contract_address(), &amount);
```

So it cannot retroactively recognise money that has already arrived — calling it would
require a *second* transfer from a registered source. There was no sync path of any kind.

**The consequence was permanent.** Every fee the protocol routed to its own reserve landed
at the address and became unspendable: invisible to the solvency checks, unreachable by
`receive_yield`, with no recovery function. This is worse than the EVM equivalent, which at
least has `syncUntrackedFunds` to recover from the same push.

### Fix

Added `sync_untracked()` and `untracked()` to the reserve, mirroring what `pool-escrow`
already had. Permissionless, because it only recognises funds the contract already holds —
there is nothing to gain by calling it, and real harm in the reserve sitting insolvent on
paper because nobody did.

Verified by `a_pushed_transfer_can_be_credited_and_then_spent`: a pushed 250,000 is
invisible, is credited by the sync, raises `available_for_loan` by exactly that, and is
then actually lent out. Syncing with nothing untracked returns `NothingToSync`.

---

## ST-2 · Medium · Locked positions archive mid-term — **fixed**

**Where** `piron-types/src/lib.rs:379`, `locked-pool-manager/src/storage.rs:46`

Persistent entries live `PERSISTENT_BUMP_LEDGERS` — 60 days — from the last touch. Locked
terms are 90, 180 and 365 days, and now up to ten years. A position nobody interacts with
archives well before maturity.

Reading a position extends it, so a keeper *could* call `position(id)` once per position
per window. But that is one transaction each, and the code comment that promises this
points at a file which does not exist:

> Reads a position, extending its TTL. A locked position can outlive several bump windows,
> so the keeper re-bumps untouched ones — see `ttl.rs`.

There is no `ttl.rs` (the constants live in `piron-types`), and no keeper entry point in
any contract. So the default outcome for a long-dated position is archival.

An archived entry is restorable rather than lost, so this is not fund loss — but a holder
cannot redeem until someone restores it, which is not a state to put them in by default.

### Fix

Added `bump_positions(Vec<u64>)` to the locked manager: one batched, permissionless call
that extends many positions at once, returning how many existed. Permissionless because it
only extends the life of data already there. The stale comment is corrected.

Verified by `positions_can_be_kept_alive_in_one_call`, including that a non-existent id is
counted as untouched rather than failing the batch, and that bumping changes nothing else
about the position.

---

## ST-3 · High · Every pool's manager has an unclaimed, permanent upgrader slot — **open**

**Where** `pool-factory/src/lib.rs:162` onward, and `bind_upgrader` on all three managers

The factory deploys a **fresh manager, escrow and share token per pool**. Upgrade authority
on Soroban is claimed once, first-come, and is irrevocable:

```rust
pub fn bind(env: &Env, upgrader: &Address) -> Result<(), UpgradeError> {
    if env.storage().instance().has(&UPGRADER) {
        return Err(UpgradeError::AlreadyBound);
    }
    env.storage().instance().set(&UPGRADER, upgrader);
    Ok(())
}
```

On the managers that call is gated on the **Admin** role:

```rust
pub fn bind_upgrader(env: Env, caller: Address, upgrader: Address) -> Result<(), Error> {
    Self::require_admin(&env, &caller)?;
    piron_types::upgradeable::bind(&env, &upgrader)...
}
```

Nothing binds it. `scripts/deploy_soroban.sh` binds the seven singletons — access manager,
registry, fee manager, factory, reserve, and the two template managers — and then says:

> Every contract names the timelock as its upgrader, so no bytecode in the system can be
> replaced without passing the 72-hour delay and the guardian's brake.

That is not true of any pool, and pools are the contracts holding user funds. The factory
does not bind them, stores no timelock address, and has no entry point that could.

**The risk is not that upgrades are unavailable — it is that the slot is unclaimed.**
Whoever holds Admin at any point can bind *any* address as the permanent upgrader of a live
pool's manager, and no later call can displace it. A key compromise that would otherwise
be recoverable by rotating roles becomes permanent ownership of that pool's bytecode.

This is also a real divergence from EVM, where `Manager`, `StableYieldManager` and
`LockedPoolManager` are all `msg.sender == timelockController`. Pool logic is upgradeable
through the timelock on EVM and bound to nobody on Stellar.

### Fix — not applied in this pass

The durable fix is to **bind at construction**, closing the window entirely: thread the
timelock into each manager's constructor (via `Roles`, which the factory already passes
through, or as its own parameter) and call `bind` there. No admin action, no window, no
land-grab.

I have not applied it because it changes three manager constructors, the three factory
creation functions, the deploy script and every test fixture that constructs a manager —
a coordinated change I would rather land deliberately than fold into a review pass. Until
it does land, the operational requirement is: **immediately after creating any pool, the
admin must call `bind_upgrader(admin, timelock)` on its manager.** That requirement is
currently written down nowhere.

---

## ST-4 · Low · Upgrade documentation contradicted the deployment — **fixed**

**Where** `pool-escrow/src/lib.rs`, `pool-share/src/lib.rs`

Pool escrows and share tokens are permanently immutable in practice. For the escrow,
`bind_upgrader` requires the deployer, which for a factory-created escrow is the factory,
and the factory never calls it. `pool-share` has no upgrade functions at all.

That is **correct** and matches EVM, where `PoolEscrow.sol`, `LiquidityPool.sol` and
`StableYieldPool.sol` all revert in `_authorizeUpgrade` — the contracts recording a
holder's position should not be replaceable. The defect was only that the escrow's own
header claimed the opposite:

> each one names an upgrader once — in production the timelock — and only that address can
> swap the code.

### Fix

Corrected both headers to state the immutability, why it is intended, and the EVM
functions it matches. No behaviour changed — the behaviour was already right.

---

# Checked and found not to apply

Each of these is a confirmed finding on EVM. None of them is a defect here, and the reason
in every case is architectural.

### The cross-pool settlement defect (EVM S-1) cannot occur

On EVM, `positions` is one global mapping and `redeem`/`earlyWithdraw` bound only to the
*calling* pool, so a position could be settled against a different pool's escrow — proven
at 50,616 and 45,185 drained.

Soroban deploys **one contract instance per pool** (`create_locked_pool` calls
`deployer().deploy_v2`), and positions are keyed `Position(u64)` inside that instance's own
storage. There is no shared position space, no `pool_address` field on a position to get
wrong, and `owned_position` already checks ownership. The failure mode is not expressible.

### Retroactive penalty repricing (EVM S-3) cannot occur

On EVM, `configureLockTier` replaces a tier wholesale, and the exit penalty is read live,
so an operator could raise the cost of leaving an open position.

Soroban has only `add_tier`, which appends, and `set_tier_active`, which touches exactly
one field:

```rust
tier.is_active = is_active;
tiers.set(tier_index, tier);
```

No function mutates a tier's APY or penalty. Reading the penalty live is therefore safe
here, because the value can never change.

### Reentrancy in `fee-manager::distribute` is not exploitable

State is zeroed *after* the transfers, which on EVM would be a reentrancy pattern. Soroban
rejects reentrant calls into the same contract at the host level, so there is no path back
in. Worth knowing if this code is ever ported in the other direction.

### `loan_to_pool`'s caller-chosen recipient is already bound

Fixed in an earlier pass: the reserve now keeps a pool→escrow registry and refuses any
other recipient (`set_pool_escrow`, `require_pool_escrow`). Re-checked and holding, with
`money_cannot_be_sent_anywhere_but_the_pools_own_escrow` covering it.

---

# Checked and found sound

- **`access-manager`**: role grants go through propose-then-execute with a delay; direct
  grants and renouncing are refused; revocation is separate authority.
- **`upgrade-timelock`**: 72-hour delay, grace-period expiry, guardian pause, and the
  deploy script binds the timelock as its own upgrader so changing the timelock passes
  through the timelock.
- **`pool-escrow`** accounting: `balance()` returns tracked funds rather than the token
  balance, with a deliberate `sync_untracked` for pushed transfers — the fix that ST-1
  shows the reserve was missing.
- **`pool-share`**: mint and burn are manager-only; the token holds no opinion on NAV;
  allowances honour their expiry ledger.
- **TTL on reads**: `read_position` and `read_user` extend only on the branch where the
  entry exists, because extending a missing key is a host error.
- **Position ownership**: `owned_position` gates redeem, exit and rollover on the caller.

---

# Scope

Read in full: `single-asset-manager`, `stable-yield-manager`, `locked-pool-manager` and
their `storage.rs`, `pool-escrow`, `pool-share`, `yield-reserve`, `fee-manager`,
`access-manager`, `pool-factory`, `pool-registry`, `upgrade-timelock`, `piron-types`.

Not covered: the economics of `MAX_TIER_BPS` and the reserve split, which are parameter
choices rather than correctness questions; and fee/footprint optimisation.
