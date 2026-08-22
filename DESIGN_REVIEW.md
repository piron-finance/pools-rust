# Design review — the Stable Yield allocation model

Read end to end: `StableYieldManager` → `StableYieldEscrow` → `StableYieldNAVLibrary`, and
against how `pironBackend/src/modules/spv` and the SPV app consume it.

The allocation model is the weakest part of the protocol, and the problems compound.

---

## What happens today

| Step | Function | What moves |
|---|---|---|
| 1 | `createPendingAllocation` | Operator authorises an SPV **and the cash leaves the escrow immediately** — `escrow.allocateToSPV(spv, amount)` |
| 2 | `addInstrument` | SPV reports what it bought; `usedAmount` is drawn down |
| 3 | `returnAllocation` | SPV sends unused cash back |
| — | `expiresAt` | Read in **exactly one place**: `addInstrument` reverts after it passes |

## Why returning exists at all

Cash moves at step 1, before there is anything to buy. `returnAllocation` exists only
because the transfer was speculative — the pool hands over money against an intention, then
has to ask for the remainder back.

That is the root of everything below. `usedAmount` / `returnedAmount` reconciliation,
`MAX_PENDING_ALLOCATIONS`, the phantom NAV line, and the return path are all consequences
of moving cash before there is a holding to move it against.

## Why expiry is worse than pointless

`expiresAt` blocks `addInstrument` and does nothing else. There is no reaper. The
`AllocationCancelled` event is **declared and never emitted** — the cancel path was designed
and never built.

So an expired allocation is a permanent zombie:

- the cash is with the SPV
- `addInstrument` now reverts, so it can never resolve into a holding
- status stays `PENDING`, so `_getPendingAllocationsValue` keeps counting it
- `totalSPVAllocations` and `poolToSPVAllocations` stay elevated
- it occupies one of `MAX_PENDING_ALLOCATIONS` slots forever
- the only exit is the SPV voluntarily calling `returnAllocation`

`AllocationStatus` has no `EXPIRED` member, and nothing on-chain ever moves an allocation
out of `PENDING` when it lapses.

### It hits deployable capital twice

`isReadyForAllocation` computes:

```
effectiveReserves = realCash − pendingWithdrawals − pendingFees
requiredReserve   = max(minAbsolute, ratioBps × totalNAV)
available         = effectiveReserves − requiredReserve
```

A zombie allocation has already removed the cash from `realCash`, **and** it inflates
`totalNAV`, which raises `requiredReserve`. So it reduces what the pool can deploy from both
directions at once. That is precisely the "confusion as to what's left, reserves" — the
mechanism has a name now.

### And it misprices the pool

This is the part that turns a reporting problem into an economic one. NAV includes the
phantom allocation, so `navPerShare` is too high. **Anyone depositing while a zombie exists
buys shares at an inflated price** — they get fewer shares than the pool's real assets
justify, and the difference accrues to existing holders. Nothing ever corrects it, because
nothing removes the zombie.

## The backend is already papering over it

`pironBackend/src/modules/spv/spv.service.ts` has to synthesise the status the contract
does not have:

```ts
// :2642 — computed off-chain, because the chain has no EXPIRED status
expired: status === 'PENDING' && remainingAmount > 0n && expiresAt.getTime() <= Date.now(),
```

and elsewhere, in the same file:

```ts
// :2096 — filters on a status that can never exist. This count is permanently zero.
expired: allocations.filter((a) => a.status === 'EXPIRED').length,
```

Two different notions of "expired" in one service, one of which is dead code, because the
contract exposes neither a state nor an event for a lapsed allocation. The SPV dashboard
surfaces expired allocations with a remaining balance (`:2748`) — but there is no on-chain
action to resolve them, so the operator is shown a problem with no button.

---

## Recommendation

### Now — make expiry actionable

1. Add `EXPIRED` to `AllocationStatus`.
2. Add `cancelAllocation(bytes32 allocationId)` — operator or admin, callable once
   `block.timestamp >= expiresAt`. It sets the status, removes the allocation from
   `poolPendingAllocationIds`, and decrements `totalSPVAllocations` and
   `poolToSPVAllocations`. It emits the `AllocationCancelled` event that already exists.
3. Because `_getPendingAllocationsValue` counts only `PENDING` and `INVESTED`, cancelling
   corrects NAV in the same transaction. The mispricing stops.
4. Record the outstanding amount as an **SPV receivable**, not as pool value — the money is
   still owed, but it is a claim on a counterparty, not an asset the pool holds.

That last distinction is the real fix. Today "cash out with the SPV, unevidenced" and "cash
out with the SPV, evidenced by an instrument" are the same number in NAV. They are not the
same risk and should not price the same.

### Later — allocation as a limit, not a transfer

The cleaner model: `createPendingAllocation` records a **spending limit** and moves nothing.
Cash leaves when `addInstrument` records the holding. Then `returnAllocation` disappears,
expiry becomes harmless — a limit lapses, no cash is stranded — and NAV never carries a
phantom, because the pool holds either cash or instruments and nothing in between.

The obvious objection is operational, and it is a real one: an SPV needs cash in hand to buy
a T-bill in the world, and settlement is not atomic with an on-chain record. So this only
works if funding and purchase-reporting are separated — the SPV draws against the limit when
it actually needs to wire funds, and that draw is what creates the receivable.

Which is the hybrid, and probably where this should end up: keep the transfer, but stop
treating an unevidenced draw as if it were an asset.
