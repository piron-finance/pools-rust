# Smoke tests

One job: send a real transaction to a real deployment and watch money move. Run after
every deploy, before telling anyone the deployment is usable.

```bash
cd scripts/smoke && npm install     # once
node solana.mjs                     # devnet, or PIRON_CLUSTER=mainnet-beta
../smoke_soroban.sh testnet
```

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

`../smoke_soroban.sh` does the equivalent against a Soroban deployment: funds an investor,
deposits into a single-asset pool, and checks the escrow balance, the share supply and the
investor's share balance all agree with the amount deposited.

Neither cleans up. The pools they create are evidence, and on a test network they cost
nothing to leave behind.
