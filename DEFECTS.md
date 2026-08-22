# Defects found porting Piron to Rust

Five findings in the Solidity reference, surfaced by re-deriving the protocol against a
second runtime. Three are in the redemption path and share one root cause. None have been
fixed here — `src/` is the reference and these are decisions for whoever owns it.

Every one is reproduced by a passing test in this repo, so a fix can be verified against
the same cases.

| | Finding | Severity | Where |
|---|---|---|---|
| D1 | Matured redemption over-entitles whoever redeems first | **Critical** | `DepositWithdrawalLibrary._handleMaturedWithdrawWithFee` |
| D2 | Interest-bearing redemption pot is overstated | **High** | `DepositWithdrawalLibrary.calculateTotalReturns` |
| D3 | Emergency refunds pay par against a possibly impaired escrow | **High** | `ValidationLibrary.handleEmergencyWithdrawal` |
| D4 | Validation advertises a redemption path the router rejects | Low | `ValidationLibrary.validateWithdrawal` |
| D5 | `minimumFundingThreshold` declared as an amount, used as a rate | Informational | `IPoolTypes.PoolConfig` |

---

## The root cause behind D1, D2 and D3

**A payout is computed from state that paying it out does not change.**

Redemption divides a pot by the share supply. The supply falls as shares burn. The pot is
recomputed each call from pool fields — `actualInvested`, `totalDiscountEarned`,
`totalRaised` — that redemption never touches. So the value of a share rises every time
someone leaves, and the pool promises more than it holds.

Stable Yield does not have this bug, and the reason is instructive: it prices against
`nav_per_share`, which is derived from what the escrow actually holds. Paying someone out
lowers the escrow balance, so the price falls with the supply and the arithmetic stays
honest without anyone maintaining it. **Single Asset should work the same way.**

---

## D1 — Matured redemption over-entitles whoever redeems first

### The problem

```solidity
uint256 userShares  = IERC20(liquidityPool).balanceOf(owner);
uint256 totalShares = IERC20(liquidityPool).totalSupply();
uint256 totalReturns = calculateTotalReturns(poolData);          // constant across calls
uint256 userEntitlement = (userShares * totalReturns) / totalShares;
...
ILiquidityPool(liquidityPool).burnShares(owner, shares);          // supply falls
escrowContract.releaseFunds(receiver, netAmount);
```

`totalReturns` is fixed. `totalShares` shrinks. Every redemption raises the price of the
remaining shares.

### What it costs

With `N` equal holders and a settlement pot `T`, the holder redeeming k-th is paid
`T / (N − k + 1)`. Only the first is correct. Total demanded is `T · H(N)` — the harmonic
number — so a pool with ten holders promises about **2.9×** what it settled for.

The escrow's balance check is what stops it: `releaseFunds` reverts once the pot is gone.
So the loss does not leave the protocol, it **moves between investors**. Early redeemers
are overpaid out of late redeemers' principal, and the ones at the back are locked out
permanently. Redemption becomes a race, and the race is winnable by anyone watching the
mempool.

Reproduced live on Stellar testnet with two holders at 60/40 of a 1,052,631 pot: the first
was paid 631,578 (correct), the second was computed at 1,052,631 — the entire pot — and
the escrow refused with `#502 InsufficientBalance`. Test:
`a_discounted_deal_runs_from_funding_to_redemption`.

### The fix

**Snapshot both terms at settlement.** In `processMaturity`, record the distributable pot
and the share supply as they stand, and divide by those fixed numbers forever after:

```
distributableAtMaturity = totalReturns
sharesAtMaturity        = IERC20(pool).totalSupply()

userEntitlement = userShares * distributableAtMaturity / sharesAtMaturity
```

Neither term moves, so entitlements sum to exactly the pot regardless of who redeems when,
and ordering stops mattering. It needs two new fields and no bookkeeping on the hot path.

The alternative — decrementing a running `remainingDistributable` as it is paid — is also
correct, because numerator and denominator then fall together. It costs a write per
redemption and one more invariant to hold. Prefer the snapshot.

---

## D2 — Interest-bearing redemption pot is overstated

### The problem

Two functions share the name `calculateTotalReturns` and disagree:

| | Formula | Used by |
|---|---|---|
| `CalculationLibrary` | `fundsReturnedBySPV + (received − distributed)` | `calculateUserReturn` — what the investor is **quoted** |
| `DepositWithdrawalLibrary` | `actualInvested + totalCouponsReceived` | the redemption path — what the investor is **paid** |

For discounted pools they agree, because `processInvestment` sets
`faceValue = actualInvested + totalDiscountEarned`. For interest-bearing pools they do not.

The payout formula counts **every coupon ever received**, including those already
distributed and claimed through `claimCoupon`. Those coupons have left the escrow. Counting
them again inflates the redemption pot by the full amount already paid out — on top of D1's
division error.

Test: `the_two_total_returns_disagree_for_interest_bearing_pools`.

### The fix

**Delete the `DepositWithdrawalLibrary` copy and use `CalculationLibrary`'s.** It is the
correct one: coupons already distributed are claimable separately, so the redemption pot is
the SPV's return plus only the coupons that have not yet been handed out.

That also makes the quote and the payment the same number by construction, which is the
property that should have prevented this from being possible.

---

## D3 — Emergency refunds pay par against a possibly impaired escrow

### The problem

```solidity
require(assets <= userShares, "ValidationLib/exceeds refund amount");
shares = assets;
escrowContract.releaseFunds(receiver, assets);
```

Emergency refunds pay one asset per share, unconditionally. That is right when the pool
never left funding — the escrow holds exactly what was raised.

But `emergencyExit()` is `onlyValidPool` with no status guard, so a pool can enter
EMERGENCY from **INVESTED**. If the SPV has drawn the capital and defaults, the escrow
holds less than `totalRaised` while every holder is still entitled to par. First come,
first served; the rest get nothing. `_emitEmergencyMetrics` reports `totalRaised` as
`totalRefundable`, which will be wrong in exactly this case.

### The fix

Two changes, both small:

1. **Pro-rate against what is actually recoverable.** Snapshot the escrow balance when
   EMERGENCY is entered and divide it by the supply at that moment — the same shape as the
   D1 fix. Where nothing was lost this is par, so the funding-stage behaviour is unchanged.
2. **Report the truth.** `_emitEmergencyMetrics` should emit the recoverable balance, not
   `totalRaised`.

This is also where the yield reserve earns its place: `cover_shortfall` exists so the
difference lands on the protocol rather than on whichever investor was slowest. Wire the
emergency path to draw on it before pro-rating.

---

## D4 — Validation advertises a path the router rejects

`ValidationLibrary.validateWithdrawal` permits withdrawal from `INVESTED` once past
`maturityDate`. `DepositWithdrawalLibrary.handleWithdraw` then rejects `INVESTED`
unconditionally. The rejection governs, so a pool must be settled to `MATURED` before
anyone can redeem — but reading the validator alone tells you otherwise.

`ValidationLibrary.handleMaturedWithdrawal` is also never called from anywhere, and carries
its own copy of the D1 arithmetic.

**Fix:** drop the `INVESTED` clause from `validateWithdrawal`, and delete
`handleMaturedWithdrawal`. Neither changes behaviour; both remove a trap for the next
person to read this.

---

## D5 — `minimumFundingThreshold` declared as an amount, used as a rate

Declared `uint256` alongside genuine amounts, but consumed as basis points:
`targetRaise * minimumFundingThreshold / 10000`. `Manager.sol:185` rejects anything above
10000, so a caller passing a large absolute amount fails loudly. A caller passing a small
one — `5000`, meaning five thousand units — is silently read as 50%.

**Fix:** narrow the type to `uint16`/`uint32` and rename to `minimumFundingThresholdBps`.
Already typed as `u32` in the Rust.

---

## Action plan

Fix in the Solidity first. `piron-core` is shared by both Rust runtimes, so one correction
propagates to Stellar and Solana together; fixing it after Solana exists means doing it
three times.

| Order | Do | Why first |
|---|---|---|
| 1 | **D2** — delete the duplicate `calculateTotalReturns` | Smallest change, and it decides what the pot *is* before D1 decides how to divide it |
| 2 | **D1** — snapshot pot and supply at settlement | The critical one; depends on D2 being settled |
| 3 | **D3** — pro-rate emergency refunds, wire the reserve | Same shape as D1, so do it while the pattern is fresh |
| 4 | **D4, D5** — dead code and the type | Cosmetic, no behaviour change, cheap to batch |

Then, per change:

1. Amend the Solidity and its Foundry tests.
2. `make parity` in this repo — `check.py` will flag the changed files and name the Rust
   that mirrors each one.
3. Reflect the change in `piron-core`, where the existing tests already encode the current
   behaviour and will fail until updated. That failure is the point: it is the proof the
   fix reached both runtimes.
4. `make parity-accept` to re-baseline.

### What to add while you are in there

The three redemption defects would all have been caught by one invariant test:
**the sum of what every holder can redeem never exceeds what the pool holds.** Assert it
after settlement, after each redemption, and in the emergency path. It is cheap, and it is
the property that actually matters.
