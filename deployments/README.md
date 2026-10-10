# Deployments

Two files per Stellar network, written by the scripts above this directory:

| File | Written by | Holds |
| --- | --- | --- |
| `soroban-<network>.env` | `deploy_soroban.sh` | the six singletons, the wasm hashes, the role accounts |
| `soroban-<network>-pools.env` | `create_soroban_pool.sh` | one block per pool, appended |
| `solana-<cluster>.env` | `deploy_solana.sh` | the three program ids and the upgrade authority |

They are gitignored — addresses are per-environment, and a stale one in version control is
worse than none. **`../DEPLOYMENTS.md` is the committed record**: it carries the current
testnet and devnet addresses, where the interfaces are, and what the backend has to change
to use them. Keep it in step when you deploy.

Pools append rather than overwrite, because there will be many and the previous one is not
superseded by the next.
