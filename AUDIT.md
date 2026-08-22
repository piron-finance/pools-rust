# Piron Solidity — audit findings

Review of `piron-finance/Liquidity-pool` @ `d8994ce`, all three pool types plus shared
infrastructure, cross-checked against what the backend and the investor app actually call. Separate from `DEFECTS.md`, which covers what the Rust port surfaced in the
Single Asset redemption path.

Nothing here is fixed. PR #29 covers only the projection bug (D1/D2 in `DEFECTS.md`).

| | Finding | Severity | Status |
|---|---|---|---|
| C1 | `emergencyRedeem` lets an operator move any holder's value to any address | **Critical** | fixed in #30 |
| A4 | A fully impaired pool cannot be recapitalised | Medium | open |
| A6 | `timelockController` is immutable with no recovery path | Low | open, by design |
| ~~A1~~ | ~~A zero `couponFrequency` freezes a Stable Yield pool~~ | — | **withdrawn** |
| ~~A2~~ | ~~Withdrawal queue iterates unbounded~~ | — | **withdrawn** |
| ~~A3~~ | ~~Position and debt loops iterate unbounded~~ | — | **withdrawn** |
| ~~A5~~ | ~~Locked and Stable Yield have no pause~~ | — | **withdrawn** |

## Four findings withdrawn, and why

I found these by grepping for shapes — division sites, `for` loops, missing `require`s —
and reasoned about severity without reading the enclosing functions or following
delegation. Every one was wrong. Recording them so the mistake is visible rather than
quietly deleted.

**A1 — a zero `couponFrequency` freezes the pool.** `addInstrument` delegates to
`_addInstrumentWithAllocation`, which already rejects it, and `test_couponFrequencyValidation`
already covered it. I read the first part of `addInstrument`, saw only allocation checks,
and stopped.

**A2 — the withdrawal queue iterates unbounded.** `_processQueue` is already bounded:
`maxRequests`, a persisted `queue.head` cursor, a break on insufficient liquidity, and a
draw on the yield reserve before giving up. It is the design I "recommended" — already
written. The two unbounded loops I found are in `view` functions, which run off-chain.

**A3 — position and debt loops iterate unbounded.** `batchMaturePositions`,
`batchExecuteRollovers` and `settlePoolDebt` all take `uint256[] calldata` — the caller
chooses the length and pays for it. `getSettleableDebt` is a view. The only loop over
protocol state is `transferPositionOwnership` walking one user's own position list, where
the cost falls on that user's own transaction.

**A5 — no pause on Locked or Stable Yield.** I grepped the *managers* and found no
`Pausable`. The *vaults* carry it, which is where deposits and withdrawals are entered:
`LockedPool` guards 5 functions, `StableYieldPool` 4, `LiquidityPool` 6. The pause exists;
what was genuinely wrong was who could release it, fixed in #30.

The findings that held up are the ones I verified by reading the whole path or by writing a
test that failed without the fix. The lesson is in that sentence.

---|---|---|---|
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
