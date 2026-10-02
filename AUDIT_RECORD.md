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

## Second review, and what it changed

A full pass across all three runtimes, each audited on its own terms rather than by
inferring from one to the others. That distinction earned its keep twice: one critical
exists only on Solana and has no analogue elsewhere to reason from, and two findings
initially assumed to be shared turned out to be correctly scoped only after being checked
against each runtime's own code.

Every finding below was reproduced by an executable probe before being fixed. The probes
are kept as regression tests with their assertions inverted.

### Critical

- **A coupon could be claimed twice by moving shares.** Entitlement was struck on the
  holder's live token balance and reduced by a per-address claim record. Shares are a
  transferable token with no transfer hook on any of the three chains, so moving them
  reset the claim against them. Proven on all three: 200,000,000 distributed paid out
  300,000,000 on Solana, 2,000 paid 3,000 on EVM, 25,000 paid 30,000 on Soroban — the
  excess coming out of principal. Entitlement now follows a subscribed-shares record that
  only the program writes. The trade is that coupon rights no longer travel with a
  secondary-market transfer; binding them to the shares needs a transfer hook, which on
  Solana is a Token-2022 decision rather than a program change.
- **Solana's stable-yield NAV never accrued.** `total_nav` was a counter written on
  mutation, and the binding never called the valuation engine, so a holding accreted
  invisibly and the price jumped at settlement. A depositor arriving on day 89 of 90 took
  half the term's yield for one day held. NAV is now computed from the instrument book on
  every value-touching instruction, with a permissionless `refresh_nav_mirror` for readers
  since Solana has no view functions.
- **The caller named the account fees were paid into.** `treasury_asset` was constrained
  only by mint in the Solana single-asset `Withdraw` and locked `ExitEarly` contexts, so a
  holder passed their own account and kept the fee. Every withdrawal fee and early-exit
  penalty was optional. No EVM or Soroban analogue — there the destination comes from
  contract state. The single-asset pool had no treasury field at all; it has one now.

### High

- **Distributed coupons were stranded at settlement.** Claims required `Invested`, and the
  settlement pot counts only *undistributed* coupons, so anything a holder had not claimed
  was reachable by nobody. Conservation now holds: after every holder exits, custody ends
  at zero, where 10,000 used to sit.
- **The SPV could confirm less than it drew**, leaving the difference unrecorded as owed
  while every valuation was struck on the smaller figure.
- **The reserve paid a caller-chosen recipient.** `loan_to_pool` and `cover_shortfall`
  took the pool to charge and the address to pay as independent arguments. The same defect
  was already fixed on the Solidity side; the Soroban port carried the shape but not the
  fix. There is now a registry of which escrow belongs to which pool.
- **`PoolEscrow::balance()` returned the raw token balance**, so tokens sent straight to
  the address counted as pool assets — the vault inflation setup. It now returns tracked
  funds, with `sync_untracked()` for anything that arrives by bare transfer. Knock-on
  caught by the tests: reserve loans transfer directly into the escrow, so
  `borrow_from_reserve` now credits them deliberately.

### Found while fixing, not in the review

- **The coupon divisor had to freeze at settlement.** Once holders burn shares to redeem,
  dividing the coupon pot by a live supply hands each successive holder a larger slice of
  the same money — the Soroban escrow ran dry mid-probe. The same trap the redemption path
  already avoided with `shares_at_maturity`.
- **A holder record cannot be created inside the instruction that checks it.** The revert
  rolls the creation back, so `init_if_needed` on the withdrawal path would have blocked
  transferees permanently rather than merely stranding them. Hence `register_holder`.

### Medium and low

- Shares received by transfer were unredeemable on Solana (no holder record) and waved
  straight past the holding period on Soroban (a missing record read as zero). Both now
  register, with the clock starting on arrival — the rule EVM already applied in its
  ERC-20 `_update` hook. This does not fully bind the period to the shares: an address
  could register early and receive shares later, which needs the same transfer hook.
- An unpaid coupon accrued past its due date without bound; capped at one period.
- `approve` accepted `expiration_ledger` and discarded it, making every approval
  permanent behind a signature that granted otherwise.
- The queue payout loop was driven by payment count rather than queue position, which
  would have misaligned payees had the head ever rested on a served entry. Latent, not
  live, and now structurally impossible.
- Tier durations are bounded at ten years, alongside the existing APY and penalty caps.

### Left as found

**Interest-bearing instruments are marked at face from day one.** `purchase_price` is
ignored, so an SPV buying below par books an instant gain it has not earned and may never
realise. This is deliberate parity with the Solidity and reversing it changes the
economics of every interest-bearing pool, so it wants a decision rather than a patch.


## Closed since: instruments marked at face from day one

Left open through two earlier passes as a product decision, because reversing it changes
the economics of every interest-bearing pool. Closed now, because the behaviour is wrong
on its own terms rather than merely aggressive.

`interest_bearing_value` ignored `purchase_price` and returned face plus accrued coupon, so
an instrument bought below par was marked at redemption value the moment it was recorded.
An SPV lifted NAV simply by buying cheaply, the gain was unearned until the bond actually
redeemed, and a default beforehand read as a fall from a level the pool had never held.

The capital leg now accretes from what was paid toward face over the holding period —
exactly `discounted_value` — with the coupon accrued on top. Both instrument types now
agree about what holding something below par is worth.

Applied in `piron-core` and `StableYieldNAVLibrary`, so all three runtimes move together.

Worth noting why it had survived this long: every existing test bought at par, where the
two behaviours are identical. The change broke nothing, which is exactly why it needed new
coverage rather than a green suite. Verified on both sides by reverting the fix and
confirming the new tests catch it — on Solidity, NAV jumped from 194,000 to 204,000 the
instant the purchase was recorded, the whole 10,000 discount booked unearned.


## Open

**Mid-deal SPV default.** `emergencyExit` is now guarded to the window where the escrow is
still whole, because par refunds cannot be honoured outside it. Handling a default *after*
the SPV has drawn the funds needs refunds pro-rated against what the escrow actually holds,
plus a reserve backstop for the gap. That is a mechanism to build, not a guard to loosen.

**External audit.** Everything above is internal review. It is not a substitute.
