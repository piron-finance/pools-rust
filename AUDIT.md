# Piron Solidity — audit findings

Review of `piron-finance/Liquidity-pool` @ `d8994ce`, all three pool types plus shared
infrastructure, cross-checked against what the backend and the investor app actually call. Separate from `DEFECTS.md`, which covers what the Rust port surfaced in the
Single Asset redemption path.

Nothing here is fixed. PR #29 covers only the projection bug (D1/D2 in `DEFECTS.md`).

| | Finding | Severity | Contract |
|---|---|---|---|
| C1 | `emergencyRedeem` lets an operator move any holder's value to any address | **Critical** | `StableYieldPool` |
| A1 | A zero `couponFrequency` permanently freezes a Stable Yield pool | **Critical** | `StableYieldManager` |
| A2 | Withdrawal queue iterates unbounded — gas DoS | **High** | `StableYieldManager` |
| A3 | Position and debt loops iterate unbounded | **High** | `LockedPoolManager`, `LockedPoolManagerLib` |
| A4 | A fully impaired pool cannot be recapitalised | Medium | `StableYieldManager` |
| A5 | Neither Locked nor Stable Yield has any emergency state or pause | Medium | both managers |
| A6 | `timelockController` is immutable with no recovery path | Low | four contracts |

---

## C1 — `emergencyRedeem` lets an operator move any holder's value to any address

Found by checking what the investor app calls for the non-Single-Asset pools.

### The problem

[StableYieldPool.sol:272](src/managed/StableYieldPool.sol#L272):

```solidity
function emergencyRedeem(uint256 shares, address receiver, address owner) external returns (uint256 assets) {
    require(accessManager.hasRole(accessManager.OPERATOR_ROLE(), msg.sender), "StableYieldPool/not operator");
    ...
    _burn(owner, actualShares);
    if (immediate) { escrow.withdraw(receiver, withdrawalValue); ... }
}
```

The caller names **both** `owner` and `receiver`, and they need not be the same address.
There is no allowance check, no signature from `owner`, and no requirement that
`receiver == owner`.

So anyone holding `OPERATOR_ROLE` can burn any holder's shares and send the proceeds
anywhere. That is not an emergency redemption, it is a confiscation primitive.

Compare `LiquidityPool.emergencyWithdraw()`, which takes no arguments and acts on
`msg.sender` — self-service, and correct.

It is also the **only** exit on `StableYieldPool` without `whenNotPaused`. `deposit`,
`mint`, `withdraw` and `redeem` all carry it. So an operator can exit a chosen holder while
the pool is frozen to everyone else — the precise asymmetric-freeze hole that lets one
party out at a stale price while the rest wait for the correction.

The investor app exposes this function via `usePoolExit`.

### The fix

Three changes, all small:

1. **Act on the caller.** Drop `owner`; burn `msg.sender`'s shares. If a delegated exit is
   genuinely needed, require an ERC-20 allowance from `owner` the way `transferFrom` does.
2. **Pay the owner.** Require `receiver == owner`, or drop `receiver` entirely. An
   operator-triggered exit should never choose the destination.
3. **Add `whenNotPaused`.** A paused pool means paused for everyone.

If the intent was an operational tool to force-exit a sanctioned or compromised holder,
that is a different function with a different name, and it should pay into escrow or a
quarantine address — never to an operator-chosen recipient.

## A1 — A zero `couponFrequency` permanently freezes a Stable Yield pool

### The problem

`addInstrument` ([StableYieldManager.sol:520](src/StableYieldManager.sol#L520)) takes
`uint8 couponFrequency` and validates the allocation thoroughly — existence, pool match,
SPV identity, status, expiry, size — but **never validates `couponFrequency` itself**.

NAV then divides by it, twice, in
[StableYieldNAVLibrary.sol:102 and :108](src/libraries/StableYieldNAVLibrary.sol#L102):

```solidity
uint256 couponPeriodSeconds = SECONDS_PER_YEAR / instrument.couponFrequency;
uint256 couponAmount = (instrument.faceValue * instrument.annualCouponRate)
                     / (10000 * instrument.couponFrequency);
```

An `INTEREST_BEARING` instrument added with `couponFrequency = 0` makes every NAV
computation revert. `calculateGrossAssetValue` walks all active instruments, so one bad
entry poisons the whole calculation.

### What it costs

NAV is on the path of nearly everything. Once it reverts:

- **deposits** revert — `navPerShare` is read to size the mint
- **withdrawal requests** revert — the same read prices the quote
- **queue processing** reverts
- **every view** a frontend or the backend indexer calls reverts

The pool is frozen and **investors cannot exit**.

The only escape is `matureInstrumentWithFunds` ([:606](src/StableYieldManager.sol#L606)),
the sole path that sets `isActive = false` — and it requires *both*
`block.timestamp >= instrument.maturityDate` and a non-zero `maturityAmount` from the SPV.
So the freeze lasts until the bad instrument's maturity date, which the same call chose,
and then only if the SPV cooperates. A ten-year maturity is a ten-year freeze.

`deactivatePool` ([:780](src/StableYieldManager.sol#L780)) does not help — it stops new
pool activity but does not stop NAV being read on the exit paths.

This does not require malice. Typing an instrument `INTEREST_BEARING` and passing `0` for a
zero-coupon holding is an easy mistake, and the transaction succeeds.

### The fix

Reject it at the door, in `addInstrument`:

```solidity
if (instrumentType == InstrumentType.INTEREST_BEARING) {
    if (couponFrequency == 0) revert InvalidCouponFrequency();
    if (annualCouponRate == 0) revert InvalidCouponRate();
}
```

And defensively in the NAV library — return `faceValue` with no accrual rather than
dividing by zero — so one bad row can never take the pool's pricing down with it.

Worth adding an admin path to deactivate an instrument without funds, for exactly this
class of mistake. Today there is no way to take a bad row out of NAV.

---

## A2 — Withdrawal queue iterates unbounded

[StableYieldManager.sol:940 and :999](src/StableYieldManager.sol#L940):

```solidity
for (uint256 i = queue.head; i < queue.tail; i++) {
```

Processing walks the entire queue in one transaction. The queue grows with user demand and
nothing caps it. Past a few hundred entries the loop exceeds the block gas limit and
**processing can never complete** — the queue is stuck and no one is paid.

It is also cheap to force: a griefer submits many small requests to push the queue past the
limit.

**Fix:** process a bounded batch and advance `head` across calls. Stop at the first request
that cannot be funded so ordering is preserved. This is the shape the Rust port uses
(`MAX_WITHDRAWAL_BATCH`), forced there by Soroban's footprint declaration — but it is a
live gas-DoS on EVM too.

---

## A3 — Position and debt loops iterate unbounded

Six sites: [LockedPoolManager.sol:403, :486](src/LockedPoolManager.sol#L403) and
[LockedPoolManagerLib.sol:123, :162, :173, :201](src/libraries/LockedPoolManagerLib.sol#L123).

They walk `positionIds`, `allDebtIds` and `oldOwnerPositions`. A depositor with many small
positions can make their own operations un-runnable; `allDebtIds` is protocol-wide, so if
that one is on an operator path it is a protocol-level stall.

**Fix:** the same treatment — paginate, or index so the full scan is never needed.
`allDebtIds` in particular should be settled in batches.

---

## A4 — A fully impaired pool cannot be recapitalised

[StableYieldManager.sol:753](src/StableYieldManager.sol#L753) guards the empty case:

```solidity
uint256 navPerShare = totalShares == 0 ? 1e18 : (totalNAV * 1e18) / totalShares;
```

but not the *worthless* case. If `totalNAV` reaches zero with shares still outstanding —
total SPV default — `navPerShare` is zero, and [:287](src/StableYieldManager.sol#L287)
divides by it:

```solidity
shares = (netDepositAmount * 1e18) / navPerShare;
```

So a pool that has lost everything cannot take a deposit, which is precisely when someone
might want to recapitalise it. Same at [:1046](src/StableYieldManager.sol#L1046).

**Fix:** decide the intended semantics and encode it. Either refuse deposits into a
worthless pool with a clear error, or floor `navPerShare` at 1 so recapitalisation is
possible and existing holders are diluted to nothing — which is the economically correct
outcome, but should be a deliberate choice rather than a revert.

---

## A5 — Neither Locked nor Stable Yield has an emergency state or a pause

`Manager` (Single Asset) has `EMERGENCY` status, `cancelPool`, and `whenNotPaused` on its
entry points. `LockedPoolManager` and `StableYieldManager` have **none of these** — no
`Pausable`, no `whenNotPaused`, no emergency status. I grepped both.

So if an SPV defaults on a locked pool there is no way to halt redemptions and no
pro-rating. `LockedPoolEscrow.withdraw` simply reverts once short, and positions redeem
first-come-first-served until the balance runs out — the same race as `DEFECTS.md` D1,
reached from a different direction.

**Question before a fix:** is this deliberate — SPV default absorbed by the yield reserve,
which both pools are wired to — or an oversight? If the reserve is meant to cover it, the
draw is not automatic today and should be.

---

## A6 — `timelockController` is immutable with no recovery path

Set once in `initialize` on `Manager`, `StableYieldManager`, `LockedPoolManager` and
`PoolRegistry`, with no setter. That is the right default: no admin can swap the timelock
to bypass the delay.

The tradeoff is that a wrong address at deployment makes the contract permanently
unupgradeable, and a compromised timelock cannot be replaced. Worth a deliberate decision
rather than an accident — a timelocked setter for the timelock itself is the usual answer.

---

## Checked and sound

Recording these so the next reviewer does not repeat the work:

- **Upgrade authorisation.** All 14 upgradeable contracts carry `_authorizeUpgrade`.
  Managers and registry gate on `timelockController`; `FeeManager` and `YieldReserveEscrow`
  on `MULTISIG_ADMIN_ROLE`; every vault and pool escrow disables upgrades outright with a
  revert. No gaps.
- **Initialisation.** All 14 have `_disableInitializers()` in the constructor. No
  implementation can be initialised directly.
- **Escrow access control.** Every `safeTransfer` path across the four escrows is behind a
  modifier or an inline `require`. The two that look unguarded in
  `LockedPoolEscrow` — `recordProtocolFundsFromReserve`, `returnProtocolFundsToReserve` —
  check the caller inline.
- **Reentrancy.** `nonReentrant` is on the fund-moving paths; token transfers use
  `SafeERC20`.
