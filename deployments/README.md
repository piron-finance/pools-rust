# Deployments

`make deploy NETWORK=testnet` writes `soroban-<network>.env` here: contract addresses and
the role accounts they were wired with. The backend and the apps read these.

They are gitignored — addresses are per-environment, and a stale one in version control is
worse than none.
