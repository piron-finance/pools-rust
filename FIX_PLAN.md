# Fix plan — Piron Solidity

Ordered work packages for the findings in `DEFECTS.md` (redemption path), `AUDIT.md`
(contract review) and the two below, found by reading how the backend and apps actually
use the contracts.

PR #29 is open and covers the projection bug only.

---

## Two findings from the off-chain review

### B1 — The backend silently misses two of the three withdrawal paths · **High**

`DepositIndexer` indexes Single Asset withdrawals off `WithdrawalFeeCollected`, with this
comment:

> `WithdrawalFeeCollected(pool, owner, feeAmount, netAmount)` is emitted unconditionally on
> every withdrawal

**It is not.** That event is emitted at exactly one site — `DepositWithdrawalLibrary:138`,
inside `_handleMaturedWithdrawWithFee`. The other two paths emit only `Withdraw`:

| Path | Emits | Pool indexed? | Indexed? |
|---|---|---|---|
| Matured redemption | `Withdraw` + `WithdrawalFeeCollected` | yes | **yes** |
| Funding cancellation (`handleFundingWithdrawal`) | `Withdraw` only | **no** | **no** |
| Emergency refund (`handleEmergencyWithdrawal`) | `Withdraw` only | **no** | **no** |

`Withdraw(address indexed caller, address indexed receiver, address indexed owner, ...)`
has all three topic slots spent on addresses and **carries no pool**, so the indexer cannot
filter it per pool and does not consume it.

**Consequence:** every funding-stage cancellation and every emergency refund is invisible
to the backend. Positions and balances in the database drift from chain state, and a user
who cancelled during funding still shows as a holder. This is live across nine chains
today.

**Fix:** give every withdrawal path an event carrying the pool as topic 1. Cleanest is one
event emitted by all three:

```solidity
event PoolWithdrawal(
    address indexed pool,
    address indexed owner,
    uint8   indexed kind,      // 0 funding, 1 matured, 2 emergency
    uint256 assets,            // net to the receiver
    uint256 shares,
    uint256 fee
);
```

Keep the ERC-4626 `Withdraw` for standards compliance; add this for indexing. The backend
then has one filter for all three, and `kind` removes the guessing.

### B2 — A live UI button that can never succeed · **Medium**

`app/(dashboard)/pool/[id]/page.tsx:1285` renders a **Claim refund** button calling
`LiquidityPool.claimRefund()`. That function requires `pendingRefunds[msg.sender] > 0`, and
nothing ever writes that mapping — `setPendingRefund` is `onlyManager` and the Manager
never calls it. The button reverts 100% of the time.

This is `DEFECTS.md` D7 with a user pointing at it. Whatever is decided about the refund
machinery, the UI needs to stop offering it in the meantime.

---

## The plan

Ordered by severity × independence × size. Each row is one PR.

| # | Closes | What | Size | Blocked on |
|---|---|---|---|---|
| 1 | — | **PR #29**, projection bug | done | review |
| 2 | A1 | Validate `couponFrequency`; defensive NAV branch; admin instrument deactivation | S | — |
| 3 | D1, D6 | Fix the redemption divisor | S | #29 |
| 4 | B1 | One indexable withdrawal event on all three paths | S | — |
| 5 | A2, A3 | Paginate the withdrawal queue and the position/debt loops | M | — |
| 6 | A4 | Decide and encode worthless-pool semantics | S | a decision |
| 7 | D3, A5 | Emergency semantics across all three pool types | M | three decisions |
| 8 | D4, D5, D7, B2 | Dead code, the bps type, the refund path | S | one decision |

### 2 — A1, the frozen pool · start here

Smallest critical fix, no interaction with anything else.

- `StableYieldManager.addInstrument`: reject `couponFrequency == 0` and
  `annualCouponRate == 0` when `instrumentType == INTEREST_BEARING`.
- `StableYieldNAVLibrary.calculateInterestBearingValue`: if `couponFrequency == 0`, return
  `faceValue` with no accrual instead of dividing. One bad row must never be able to take
  the pool's pricing down.
- Add an admin path to set `isActive = false` on an instrument without requiring funds or
  maturity. There is no way to pull a bad row out of NAV today.

**Test:** add an instrument with `couponFrequency = 0`; NAV, deposit and withdrawal all
still work; the instrument can be deactivated by an admin.

### 3 — D1, the divisor

```solidity
// processMaturity
sharesAtMaturity = IERC20(pool).totalSupply();

// _handleMaturedWithdrawWithFee
userEntitlement = userShares * totalReturns / sharesAtMaturity;
```

Same treatment for `calculateUserDiscount` and the `EMERGENCY` branch of
`calculateUserReturn` (D6), which read the live supply for the same reason.

**Test:** invert `test_maturity_multipleHolders_stillRacesOnTheLiveDivisor` — both holders
paid pro rata, escrow ends empty. Add the invariant assertion below.

### 5 — A2, A3, the loops

Bounded batch plus a cursor, stopping at the first request that cannot be funded so
ordering holds. The Rust port already works this way (`MAX_WITHDRAWAL_BATCH`); the shape
carries over.

`allDebtIds` is the one to watch — it is protocol-wide, so its loop is a protocol-level
stall rather than a per-user one.

### 7 — D3, A5, emergencies

Blocked on decisions. See below.

---

## Decisions needed before 6, 7 and 8

**A4 — what should a worthless pool do?** `navPerShare` is zero when `totalNAV` hits zero
with shares outstanding, so deposits revert — precisely when someone might recapitalise.
Refuse with a clear error, or floor the price at 1 and dilute existing holders to nothing?
The second is economically right and should be chosen deliberately, not arrived at by a
revert.

**D3 / A5 — is post-investment emergency in scope?** Single Asset can only reach EMERGENCY
pre-investment today, because `emergencyExit()` has no caller. Locked and Stable Yield have
no emergency state at all. If SPV default is meant to be absorbed by the yield reserve, the
draw is not automatic today and should be. If it is meant to halt and pro-rate, all three
pool types need the mechanism.

**D7 / B2 — refund path: delete or finish?** If finished, the double-burn in `claimRefund`
must be fixed first. If deleted, the UI button goes with it.

**Q1 from earlier — is discount ever separately claimable?** If yes, discounted pools
acquire the double-count that PR #29 just removed from interest-bearing ones.

---

## One thing to add regardless

Every redemption defect here would have been caught by a single invariant:

> the sum of what every holder can redeem never exceeds what the pool holds

Assert it after settlement, after each redemption, and on the emergency path, for all three
pool types. Cheap, and it is the property that actually matters.
