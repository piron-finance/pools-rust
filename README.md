# Piron — Rust

The Piron Pools protocol on **Stellar** and **Solana**. The Solidity implementation lives
in its own repository and is the reference; this one follows it.

```
crates/piron-core/     the protocol: math, state machines, validation.
                       no_std, zero dependencies, allocates nothing, knows no chain.
soroban/               Stellar bindings. Storage tiers, TTL, require_auth, SEP-41.
solana/                Solana bindings. Accounts, PDAs, signers, SPL.
parity/                drift tracking against the Solidity repo.
scripts/               deployment.
```

## Why one repo for two chains, separate from the Solidity

`piron-core` holds every decision the protocol makes and is shared verbatim by both Rust
targets. Splitting Stellar and Solana apart would mean publishing that crate or pinning it
by commit — friction on the file that changes most, in exchange for nothing.

The real seam is the toolchain. Solidity is Foundry; both of these are Cargo. Splitting
there gives each side its own CI, its own dependency graph, and an audit scope that matches
how the code is actually reviewed: a Rust auditor reads the core and both bindings
together, because a bug in the core is a bug on both chains.

## Getting started

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32v1-none
cargo install --locked stellar-cli

make test      # logic and contracts
make build     # wasm
make deploy    # testnet
```

`make test` needs no chain and no network for the core; contract tests run against the
Soroban host in-process.

## Keeping up with the Solidity

```bash
make parity           # what changed in the reference that we have not reflected
make parity-accept    # re-baseline once it is reflected
```

`parity/check.py` hashes every `.sol` file in the reference repo and names the Rust that
mirrors each one. It finds that repo via `parity/config.json`, `$PIRON_SOLIDITY_ROOT`, or
`--solidity`. `parity/sync_errors.py` regenerates the bindings' error enums from
`piron-core`, so one protocol condition reports the same number on every runtime.

Conventions that are not optional are in `.claude/skills/piron-multichain/SKILL.md`.
Architecture in `STELLAR_ARCHITECTURE.md` and `SOLANA_ARCHITECTURE.md`. The task queue and
the open questions found while porting are in `IMPLEMENTATION_PLAN.md`.
