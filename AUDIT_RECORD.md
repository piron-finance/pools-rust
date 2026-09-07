# Audit record

What the Solidity review found, and where each item ended up. This replaces `AUDIT.md`,
`DEFECTS.md`, `FIX_PLAN.md` and `DESIGN_REVIEW.md`, which were working documents from the
review and described a state of the code that no longer exists. Git history has them.

Reference: `piron-finance/Liquidity-pool` PR #34, merged as four commits. Every item below
is closed unless the last column says otherwise.

## The redemption path

| | Finding | Where it landed |
|---|---|---|
| D1 | Matured redemption divided by live share supply, so each holder claimed a larger slice of a draining pot and the last one out was short | `sharesAtMaturity` frozen at settlement; both chains |
| D2 | The pot was built from projections — face value, scheduled coupons — rather than what the SPV returned | One formula, from actuals; both chains |
| D3 | Emergency refunds pay one unit per share against an escrow that may already be drawn down | `emergencyExit` now requires `fundsWithdrawnBySPV == 0`; both chains |
| D4 | The withdrawal gate admitted `INVESTED` past maturity; the router rejects it regardless | Clause removed; both chains |
| D5 | `minimumFundingThreshold` declared as an amount, used as a rate | Validated `<= 10000` in Solidity, typed `u32` in Rust |
| D6 | The same live divisor in the quoted-value view, so the quote and the payment disagreed | Both use the frozen count |
| D7 | `claimRefund` was unreachable — `setUserRefund` was never called — and had a live button pointed at it | Machinery deleted; button now calls the working emergency exit |

## Contract review

| | Finding | Where it landed |
|---|---|---|
| C1 | `emergencyRedeem` let an operator burn any holder's shares and name themselves the recipient | `receiver` argument removed; proceeds go to the holder |
| A4 | A pool at zero NAV with shares outstanding cannot take deposits | Not a defect. `navPerShare == 0` makes the deposit *revert* rather than mint wrongly, and `deployProtocolCapital` raises NAV and restores it. Recorded because the original finding claimed the pool could not be recapitalised, which is wrong. |
| A6 | The timelock can only be replaced by itself | Not a defect. That is the design; losing the timelock bricking upgrades is inherent to timelocked governance. |
| — | Four findings withdrawn during review after reading the enclosing functions rather than grepping for shapes | See PR #34 discussion |

## The allocation model

The Stable Yield allocation lifecycle — expiry clock, status machine, return-gated-on-status
— modelled the SPV as an external counterparty. It is Piron's own fund management arm, so
the machinery enforced a constraint nobody wanted and created states nobody designed. One
of them stranded capital: a partially deployed allocation whose first instrument matured
left the remainder out of NAV, un-investable and un-returnable. Measured at 18,500 lost on
a profitable maturity.

Replaced by a capital balance per pool and SPV, with three transitions and the invariant
that every decrement is matched by an increase in another line in the same transaction:

    NAV = instruments + escrow reserves + undeployed capital

Soroban reached the same model independently and never carried the lifecycle.

## Found while hardening, not in the original review

- Interest accrued past maturity without bound, so an unsettled instrument inflated NAV
  forever. Capped at the maturity date on both chains.
- `add_instrument` clamped in-flight capital at zero, so recording a purchase larger than
  the capital drawn left NAV counting value the pool never had. Now rejects.
- No ceiling on coupon rate or frequency. Both capped.
- Three escrow paths transferred tokens while clamping a counter to zero instead of
  reverting; the locked-pool one was load-bearing, since payouts are principal *plus*
  interest and only principal was tracked.
- `YieldReserveEscrow` took the destination for outbound transfers from the caller, while
  already maintaining the set of valid escrows and enforcing it inbound.
- Fee revenue routed to the reserve arrived by bare transfer and was never credited to
  `totalBalance`, so the income meant to keep the reserve solvent could not be spent.
- Withdrawals were indexed off `WithdrawalFeeCollected`, which only fires on the matured
  path, silently dropping funding-phase and emergency exits.

## Found while writing the scenario suites

The withdrawal queue was the substantial one. Three defects, one cause: both Rust runtimes
burned a leaver's shares when the queue *reached* them rather than when they *asked*, and
re-priced the request at service time. The Solidity reference does neither.

- **The queue could be bricked permanently.** Shares are a transferable token. A holder who
  queued and then moved their shares on left a request whose burn must fail; the burn sat
  inside the batch, so the whole transaction reverted and the head could never advance past
  them. Every request behind them was stranded for good. Shares now burn when the request is
  made, which is also what the Solidity does.
- **The same shares could be queued twice**, for the same reason.
- **Re-pricing at service time paid the leaver out of everyone else.** With shares already
  gone from the supply, a fresh quote divides the pool by a denominator those shares had
  left — so the longer the queue took to reach a holder, the more they were paid. Each
  request is now paid the quote it was struck with.

Two consequences had to follow for the arithmetic to hold. NAV is now net of what the pool
already owes queued leavers, so queueing an exit does not move the price for anyone else —
asserted directly, on both chains. And the pending total carries the *gross* quote, because
the fee leaves the vault too; reserving only the net let each queued exit nudge the price up
for everyone behind it.

Also on the queue: cash standing against queued exits is no longer the SPV's to draw, and
the reserve floor no longer applies to serving the queue — the floor exists so holders can
leave, and withholding it from the queue it is held for is circular.

Found in the Solana bindings specifically, where the surface had drifted from Soroban:

- **`add_instrument` and `write_off_instrument` took any signer.** Anyone could record a
  holding against capital the real SPV had drawn — clearing its outstanding balance and
  taking the attribution — or write a holding out of NAV. Both now check the role, and every
  role check in that program reports 701, the number Soroban reports, rather than a generic
  signature error the backend cannot map.
- **A locked pool's upfront interest was never paid out.** Choosing "upfront" got the holder
  nothing on day one and principal alone at maturity: the interest simply vanished.
- **The locked pool's deposit metrics were mis-mapped** — interest paid upfront was booked
  as interest committed, and interest still owed was booked as invested capital.
- **`process_withdrawals` takes payee accounts from the operator**, so an owner check on
  each one is the only thing between an operator and redirecting a holder's money. Added.
- The holding period was configurable but unenforced on Solana, there being no per-holder
  clock to enforce it against. One added.
- A tier could be opened but never closed, and the pool itself had no pause. Both added,
  with redemptions and exits deliberately left open — closing a pool must never trap money
  already in it.

One thing the suites disproved: a doc comment claimed a locked pool holds back what the
nearest maturities need. It does not, on either chain, and deliberately — holding back
principal plus promised interest would leave the pool nothing to invest. The comment was
stale; the reserve is what covers an exit that lands before the SPV returns.

The test harnesses now refuse to run against a stale build. litesvm executes the compiled
artefact rather than the crate `cargo test` just built, so a source change with no
`cargo-build-sbf` behind it silently tested the old program and passed. That happened once
here and the guard exists because of it.

## Open

**Mid-deal SPV default.** `emergencyExit` is now guarded to the window where the escrow is
still whole, because par refunds cannot be honoured outside it. Handling a default *after*
the SPV has drawn the funds needs refunds pro-rated against what the escrow actually holds,
plus a reserve backstop for the gap. That is a mechanism to build, not a guard to loosen.

**External audit.** Everything above is internal review. It is not a substitute.
