# Defects found porting Piron to Rust

Seven findings in the Solidity reference, surfaced by re-deriving the protocol against a
second runtime. Three are in the redemption path and share one root cause. None have been
fixed here — `src/` is the reference and these are decisions for whoever owns it.

Every one is reproduced by a passing test in this repo, so a fix can be verified against
the same cases.

| | Finding | Severity | Where |
|---|---|---|---|
| D1 | Matured redemption over-entitles whoever redeems first | **Critical** | `DepositWithdrawalLibrary._handleMaturedWithdrawWithFee` |
| D2 | Interest-bearing redemption pot is overstated | **High** | `DepositWithdrawalLibrary.calculateTotalReturns` |
| D3 | Emergency refunds pay par against a possibly impaired escrow | **Latent** — see note | `ValidationLibrary.handleEmergencyWithdrawal` |
| D4 | Validation advertises a redemption path the router rejects | Low | `ValidationLibrary.validateWithdrawal` |
| D5 | `minimumFundingThreshold` declared as an amount, used as a rate | Informational | `IPoolTypes.PoolConfig` |
| D6 | The same live divisor in two views | Low | `calculateUserDiscount`, `calculateUserReturn` |
| D7 | `claimRefund` unreachable, and would double-burn if wired | Low | `LiquidityPool.claimRefund` |

---

## Each pool type prices differently, and only one of them uses NAV

Getting this straight first, because conflating them is what produced a wrong diagnosis in
the first draft of this document.

| Pool type | What a holding is | How it is priced |
|---|---|---|
| **Single Asset** | A fixed fractional claim on **one settlement** | `userShares / sharesAtSettlement` of the pot. No mark-to-market, no moving per-share price. The fraction is fixed the moment funding closes, because nothing mints afterwards. |
| **Locked** | A **discrete position** with its own term and rate | `expectedMaturityPayout`, computed at deposit. Never divided by anything. Correct by construction. |
| **Stable Yield** | A share of a **revolving portfolio** | `nav_per_share`, recomputed from live holdings. **This is the only pool type where NAV means anything.** |

## The root cause behind D1 and D3

**A divisor that the deal fixes is read live instead.**

Redemption in a Single Asset pool divides the settlement pot by `totalSupply()`. But a
Single Asset pool mints nothing after funding closes, so each holder's fraction is a
constant of the deal. Reading `totalSupply()` at redemption time reads a number that moves
for a reason unrelated to entitlement — other people leaving.

The pot does not move with it: `actualInvested` and `totalDiscountEarned` are never
decremented by a withdrawal. So the quotient climbs.

This is not a NAV problem and the fix is not a NAV. Stable Yield's live pricing is correct
*for Stable Yield* precisely because its numerator is live too — paying someone out lowers
the escrow balance, so both terms fall together. Single Asset has no live numerator and
needs none. It needs a constant denominator.

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

**Record the share supply at settlement and divide by that.** In `processMaturity`:

```
sharesAtMaturity = IERC20(pool).totalSupply();   // the deal's denominator, fixed here

userEntitlement = userShares * totalReturns / sharesAtMaturity;
```

One new field. Entitlements then sum to exactly the pot no matter who redeems when, and
ordering stops mattering. Snapshotting the pot alongside it costs nothing and makes the
invariant obvious at a glance, but the supply is the term that actually has to stop moving.

No NAV, no per-share price, no mark-to-market — none of which a fixed-term single-instrument
deal has any use for.

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

**Corrected after a second read: this is latent, not live.** `emergencyExit()` is
`onlyValidPool`, meaning only a registered pool contract may call it — and no vault does.
`LiquidityPool` never calls `manager.emergencyExit()`. Every path that actually reaches
EMERGENCY today is pre-investment: `closeEpoch` and `forceCloseEpoch` on underfunding, and
`cancelPool`, which `validatePoolCancellation` restricts to FUNDING. At those points the
escrow holds exactly `totalRaised`, so par refunds are exactly right.

The defect is therefore in what the code *permits*, not what it does. `emergencyExit()` has
no status guard, so a pool can enter EMERGENCY from **INVESTED** If the SPV has drawn the capital and defaults, the escrow
holds less than `totalRaised` while every holder is still entitled to par. First come,
first served; the rest get nothing. `_emitEmergencyMetrics` reports `totalRaised` as
`totalRefundable`, which will be wrong in exactly this case.

### The fix

Two changes, both small:

1. **Pro-rate against what is actually recoverable.** Record the escrow balance and the
   share supply when EMERGENCY is entered, and divide one by the other — the same fixed
   denominator as D1, for the same reason. Where nothing was lost this is exactly par, so
   funding-stage behaviour is unchanged.
2. **Report the truth.** `_emitEmergencyMetrics` should emit the recoverable balance, not
   `totalRaised`.

This is also where the yield reserve earns its place: `cover_shortfall` exists so the
difference lands on the protocol rather than on whichever investor was slowest. Wire the
emergency path to draw on it before pro-rating.

---

## D6 — The same live divisor appears in two more places

Found on a second read of the same pattern:

- **`Manager.calculateUserDiscount`** — `userShares * totalDiscountEarned / totalShares`.
  Identical shape to D1. It is a view, so it misreports rather than mispays: the discount
  a remaining holder is shown climbs as others redeem.
- **`CalculationLibrary.calculateUserReturn`, EMERGENCY branch** —
  `userShares * totalRaised / totalShares`. Same shape, same effect on the quote. The
  emergency *payment* is par, so the two disagree once anyone has exited.

Both take the same fix as D1: divide by the supply as of the event, not as of the call.

## D7 — `claimRefund` is unreachable, and would double-burn if it were not

`LiquidityPool.claimRefund` requires `pendingRefunds[msg.sender] > 0`. Nothing ever
populates it — `setPendingRefund` is `onlyManager` and the Manager never calls it. The same
is true of `setDiscountAccrued` and `discountedBillsAccrued`.

So the path is dead. But it is dead in a way that matters, because it burns twice:

```solidity
_burn(msg.sender, sharesToBurn);                                  // burns here
IPoolManager(manager).handleWithdraw(address(this), refundAmount, ...);  // and again inside
```

`handleEmergencyWithdrawal` burns `assets` worth of shares of its own. If anyone ever wires
`setPendingRefund` up, this becomes a live double-burn.

**Fix:** either delete `claimRefund`, `setPendingRefund`, `setDiscountAccrued` and their
state, or wire them and remove one of the two burns. Leaving half-built machinery in a
custody contract is the worst of the three options.

**Useful side effect of this finding:** because discount is *never* claimed separately, the
discounted redemption pot `actualInvested + totalDiscountEarned` does **not** double-count.
D2 is an interest-bearing problem only.

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
| 2 | **D1 + D6** — fix the divisor everywhere it is read live | The critical one; all three sites take the same one-line change |
| 3 | **D3** — pro-rate emergency refunds, wire the reserve | Same fixed-denominator fix, so do it while the pattern is fresh |
| 4 | **D7** — decide the fate of the refund and discount machinery | Needs a product decision, not just a code change |
| 5 | **D4, D5** — dead code and the type | No behaviour change, cheap to batch |

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


---

## Open questions for the protocol owner

Three things the code cannot tell me, where the right fix depends on what was intended.

1. **Was the discount meant to be claimable separately, like coupons?** `setDiscountAccrued`
   and `discountedBillsAccrued` exist and are wired into the vault but never called. If the
   intent is a separate claim, discounted pools acquire D2's double-count the moment it is
   turned on, and the redemption pot needs the same `received − distributed` treatment.

2. **Should `emergencyExit()` be callable from `INVESTED`?** It currently has no status
   guard. If a mid-deal emergency is intended — SPV default, say — then D3's pro-rating is
   required. If it was only ever meant for a failed funding round, adding the guard is the
   simpler fix and D3 largely goes away.

3. **Is the `pendingRefunds` path meant to go live?** If yes, the double-burn has to be
   fixed first. If no, deleting it removes a trap.
