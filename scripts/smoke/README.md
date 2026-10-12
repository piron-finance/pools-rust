# Smoke tests

One job: send a real transaction to a real deployment and watch money move. Run after
every deploy, before telling anyone the deployment is usable.

```bash
cd scripts/smoke && npm install          # once
node solana.mjs                          # single-asset: initialize, deposit
node solana_locked.mjs                   # locked: tier, deposit, early exit
node solana_stable_yield.mjs             # stable yield: queue account, deposit, withdrawal queue
../smoke_soroban.sh testnet              # Soroban single-asset
```

`PIRON_CLUSTER` and `PIRON_RPC` override the Solana target. The public devnet RPC
rate-limits hard, so each send simulates first and keeps those logs — a 429 on submit
discards the program's own error otherwise, which is the only part worth reading.

## Why these exist

Three programs sat on devnet for three days unable to execute a single instruction. They
declared a placeholder program id, Anchor compares the declared id against the address the
program is running at, and the mismatch failed every call with `DeclaredProgramIdMismatch`
(4100) before any handler ran. `SOLANA_FINDINGS.md` SOL-5 has the full account.

Nothing in the repository could have caught it:

- **114 unit tests passed**, because litesvm loads each program *at* its declared id, so
  the two agree in every test by construction and agree nowhere else.
- **The audit passed**, because the id is not logic and a logic review does not read it.
- **The deploy reported success**, because `solana program deploy` uploads bytecode and
  exits 0 whether or not the result can be called.

Every one of those checks the code against itself. None of them asked the chain. That is
the gap these fill, and it is the reason they encode instructions by hand from the
published IDL rather than through a generated client — a wrong IDL fails the test instead
of being papered over by the same file that generated the client.

## What they assert

`solana.mjs` creates a mint, funds an investor, derives the pool, vault and share-mint
PDAs, calls `initialize_pool`, calls `deposit`, and then reads the vault and the investor's
token accounts back from chain to confirm the money and the shares are where they should
be. It fails loudly on the first instruction that does not land.

`solana_locked.mjs` initializes a locked pool, adds a 90-day 8% tier, deposits with
interest paid upfront, and exits early — checking the holder ends down by exactly the 2%
penalty and that the penalty is in the treasury.

`solana_stable_yield.mjs` is the one that needed real work. The withdrawal queue is 22,600
bytes; a CPI can allocate at most 10,240 and a PDA cannot sign a top-level create, so the
queue cannot be both a PDA and that large. The client has to create it as a plain
program-owned account and let the pool record its address — and because nothing on the
client side did that, no stable-yield pool had ever existed on devnet. This script creates
it, initializes the pool, deposits, queues a withdrawal and processes it.

`../smoke_soroban.sh` does the equivalent against a Soroban deployment: funds an investor,
deposits into a single-asset pool, and checks the escrow balance, the share supply and the
investor's share balance all agree with the amount deposited.

## What they found when first run

The three pool types agree across chains to the unit, which is the shared `piron-core`
doing its job:

| | Stellar | Solana |
| --- | --- | --- |
| Locked, 2000 @ 90d 8%, upfront | escrow 1960547946 | vault 1960.547946 |
| Early exit payout | 1920547946 | 1920547946 |
| Exit penalty to treasury | 2% of principal | 40.000000 |
| Stable yield, 2000 queued, 0.5% fee | paid 1990000000 | paid 1990.000000 |

Neither cleans up. The pools they create are evidence, and on a test network they cost
nothing to leave behind.
