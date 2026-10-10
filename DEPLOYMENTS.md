# Deployments — Stellar and Solana

Addresses and interfaces for the two Rust chains, committed so the backend has one place
to read them from. EVM addresses are in `piron-pools/NEW_DEPLOYMENTS.md`; nothing here
touches them.

| Chain | What is deployed | Date |
| --- | --- | --- |
| Stellar testnet | Full system, 6 singletons + 3 pools | 2026-10-07, stable-yield pool 2026-10-10 |
| Solana devnet | 3 programs, no pools yet | 2026-10-07 |

Source: `piron-rust`, branch `fix/audit-findings`.

## Where the interfaces are

An interface describes code, not a deployment: one set covers every network running the
same build. Both sets are committed and were verified against the live contracts, not
assumed.

| Chain | Files | What they are |
| --- | --- | --- |
| Stellar | `abi/soroban/*.json` (11 files) | `SCSpecEntry` JSON, from `stellar contract info interface` |
| Solana | `abi/solana/*.json` (3 files) | Anchor IDLs, from `anchor idl build` |

**Stellar interfaces are byte-identical to what is live.** Each of the eleven was
regenerated from the contract id on testnet and diffed against the committed file. The
five wasm hashes the factory deploys from also match the local build byte-for-byte:

```
pool_escrow           595ef3d0c27d2e915ae80eb65609733cf382037e5d3e185410ebb480c348f448
pool_share            05357c0be8e16472897712c47aa900824451abf860f3acfbf585153016e71b1b
single_asset_manager  c453f6af15f96129ed538da44476508ac0057ef23c0688fc07acac577edde4e1
locked_pool_manager   0f8911e17c43056b58585f327a6676017caedb951fe4b5da17dd8d52587964d2
stable_yield_manager  6d50f8f816636bb9bb685b37424548496ff8513bd077ae214c67f6b161d3bc79
```

To generate a typed client rather than encode by hand:

```bash
# Stellar — a TypeScript package per contract
stellar contract bindings typescript \
  --contract-id CBAWAVQTEDQJXMJOWEPH6DVMTWJXJ42JSMDB7FADPD4P3NYOZ6OZNSUR \
  --network testnet --output-dir ./piron-factory

# Solana — the IDL is the input to @coral-xyz/anchor's Program
new Program(idl, provider)   // idl = abi/solana/piron_locked_solana.json
```

Regenerating both: `./scripts/generate_abis.sh`, or `./scripts/generate_abis.sh testnet` to
additionally diff every Soroban file against the live contract.

It also checks that each Solana program id is the same in all four places it appears —
`declare_id!`, `Anchor.toml`, the IDL's `address`, and `deployments/solana-devnet.env` —
and fails if any disagree. That check exists because `anchor idl build` rewrites
`declare_id!` and `Anchor.toml` with freshly generated keypairs when it does not like what
it finds, silently, and a mismatch there makes every instruction fail with 4100.

## Stellar testnet

| | |
| --- | --- |
| Network passphrase | `Test SDF Network ; September 2015` |
| Horizon / RPC | `https://soroban-testnet.stellar.org` |
| Explorer | `https://stellar.expert/explorer/testnet` |
| Asset | native XLM, 7 decimals, SAC `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC` |

### Singletons

| Contract | Address |
| --- | --- |
| AccessManager | `CBDVRD7PRVYF5V4BBTFLL2VEJ4S5L7WOUQVYGCQRMQLBQIXQNNGB25NV` |
| PoolRegistry | `CANW46YVGQFIAJWQXSL4GBR7YYU5OZUJZD4WGQR3OIIA3RSQWGABOVQS` |
| FeeManager | `CAWYUGAUSNEP6Z7SLOCWZEKBLYDJCAF7NVDXFG3FQYFP6YVEEKRLE5AU` |
| PoolFactory | `CBAWAVQTEDQJXMJOWEPH6DVMTWJXJ42JSMDB7FADPD4P3NYOZ6OZNSUR` |
| UpgradeTimelock | `CAMFY6QD4LLHPBSSIUMDLJK7QIHJQABQTP6PN72WVRYSRW6F4PPXTP7G` |
| YieldReserve | `CCEBZMGTAWJFFVN4JRBIXPQSLC6JZR3R5UH53N5OEAPWVL5RWOCDKM36` |

Every one of them, and the timelock itself, names the timelock as its upgrader.

### Pools

Three, one of each type. A Soroban pool is **its own set of contract instances**, not a
record inside a shared manager — the factory deploys a fresh manager, escrow and share
token per pool. So there is no single "LockedPoolManager" address the way EVM has one;
each pool's manager is its own contract, and the backend keys per pool.

| | Single asset | Locked | Stable yield |
| --- | --- | --- | --- |
| Name | Piron 90d Bill | Piron Locked | Piron Stable Yield |
| Manager | `CAQTQOH35WP3G4FDGH4GDXDKIHL63DQQV2SQNROPBTGICX3BXRAU4B3Q` | `CCPG5R27FG2QM5ZV3DWPPCWUOAEINSXW3KHIHLHLI5PPV46MJ2TMBMGD` | `CB23NPMHR2OQRTTQI56BE5B2XZVBG7CWFFG35DNNNTEFWJ5IYLS6R6FP` |
| Escrow | `CBRQ4RQO6LHGMJ5E3LCXILGFGC7WWIUNC2QCR5MBNREDMBSGDWKFQJIP` | `CBQ5IWAAELMI3V6BMTA4SM2UUKUOE3YNCNHJ3DIZFL2G44KHHSMSHALI` | `CBTG743GT7P4LOGJLOIPFSIUTDYGXTOHNNFAIG2YYFGWGFCPWM647FTJ` |
| Share token | `CBRTMSZUER5UPBRCVDNC6ZOBD4YWUFC2ETUCUROFCDW7N36UKEX5SNS2` | — | `CCKLG5SHSYR5FB2ROZ64ZYXN74GT3Q5Z5M3VYQBTHAXYR774BLDNFFFO` |
| Interface file | `abi/soroban/single_asset_manager.json` | `abi/soroban/locked_pool_manager.json` | `abi/soroban/stable_yield_manager.json` |

A locked pool has no share token: each position has its own maturity, so there is nothing
to make fungible. `create_locked_pool` returns `share: null`, and anything iterating pool
addresses has to tolerate that.

The registry agrees: `pool_count` returns 3 on both the registry and the factory, so
enumerating from either gives the same three.

### Wallets

| Role | Address |
| --- | --- |
| Admin — also holds Operator and PoolCreator | `GAKVZKZYZHKSYD7MSCSNUPWXCFOUFUQWMQXNBZ5JWGK3GIXEMZ5CJXDM` |
| Operator | `GBZLGYVKSSYPQRQO4EB6P3K2MD6ZBNGXXV4TNQGOVPY4GTURIIBRLA4G` |
| SPV | `GAGC6NSE5FLD2PRLP3YMI3NDZJJ3HB5RAONDQ6RO2JRVDER6POM4ZK73` |
| Emergency (timelock guardian) | `GCPXUZLROGMNNH7JGRSUUIX3FV7T5VB2LJ63T2XD5GDW3MKA3UXIBTQQ` |
| Multisig admin | `GAPUL3UYI7UTIJQTNENRFIVZOAS6PKSVGBEYZCUFQQOSMYXG3ZHFQTIN` |
| Treasury — every pool's fee destination | `GDVAWB6IL3C2CZRKTWPN4SU72J7UXHPXLUU4C6PNCMO7RONISGRJCU5A` |

### Creating a pool

Pools are not created by the deploy script. An admin creates one through the factory, the
same division EVM uses:

```bash
./scripts/create_soroban_pool.sh single-asset  testnet
./scripts/create_soroban_pool.sh locked        testnet
./scripts/create_soroban_pool.sh stable-yield  testnet
```

The script also does the per-pool wiring a locked pool needs, which is **two calls, both
required before an early exit can work**:

1. `access-manager grant_during_deployment --role 2` — the manager calls `loan_to_pool`,
   which requires Operator.
2. `yield-reserve set_pool_escrow --pool <manager> --escrow <escrow>` — the reserve pays
   loans only to the escrow registered for the pool being charged. Without it every loan
   fails `PoolEscrowNotSet` (#717), and the loan path is the ordinary early-exit path, not
   an edge case. The locked pool above was missing this until 2026-10-10; it is registered
   now (tx `4041666f60c4cc3bcf4630cb8cdbbade6e68366bb2f20bd8fd5ffab8f90bbb9b`).

Single-asset and stable-yield pools need no wiring at all.

## Solana devnet

| | |
| --- | --- |
| Cluster | devnet, `https://api.devnet.solana.com` |
| Explorer | `https://explorer.solana.com/?cluster=devnet` |
| Upgrade authority, all three | `7h1ZbnEPwN73cfsVbDA6ibZQ42xBbZQQnaLGZxZkVsCN` |

| Program | Id | IDL |
| --- | --- | --- |
| Single asset | `3vYqiPnknt7dpzxXmioKsfNqK8rW8oX6tDFFa1E6LVeN` | `abi/solana/piron_single_asset_solana.json` (13 instructions) |
| Locked | `5u4jwH1Rug5b356G3CxxMPbNGH21RexZbaE3wGPc6Yv6` | `abi/solana/piron_locked_solana.json` (11) |
| Stable yield | `13b1FcFBAnGEX4ocCrPp1dUx2MmbwuQEK5nHRxv75K1g` | `abi/solana/piron_stable_yield_solana.json` (13) |

**If you tried these programs before 2026-10-10 and got error 4100, that was real.** All
three were deployed declaring a placeholder program id, which Anchor checks against the
address the program actually runs at and rejects before dispatch — so no instruction could
execute on any of them. They were rebuilt with their real ids and upgraded in place at the
same three addresses, and each was then verified by sending a live instruction: all three
now reach dispatch. `SOLANA_FINDINGS.md` SOL-5 has the detail.

The addresses did not change, so nothing you recorded is stale. The IDLs did — any copy of
`abi/solana/*.json` taken before 2026-10-10 is addressed wrongly and must be replaced.

**There are no pool addresses, and that is not an omission.** A Solana pool is a PDA
derived from a pool id under its program, created when an admin calls `initialize_pool`.
There is nothing to record until one exists; derive the address rather than looking it up:

```ts
const [pool] = PublicKey.findProgramAddressSync(
  [Buffer.from('pool'), new BN(poolId).toArrayLike(Buffer, 'le', 8)],
  programId,
);
```

Positions are seeded `['position', pool, positionId]`, so a position belongs to exactly one
pool by derivation — the cross-pool class of bug is not expressible here.

### Before a Solana pool can be opened

- **The stable-yield queue account is 22,600 bytes**, over the 10,240-byte limit a program
  can allocate for itself. The client has to create and rent-fund it in the same
  transaction as `initialize_pool`. Nothing in this repo does that yet.
- **The upgrade authority is a plain keypair, not a multisig.** `SOLANA_PLAN.md:192` calls
  for a Squads v4 vault with a delay policy. On devnet a hot key is acceptable; for mainnet
  it is the difference between a 72-hour delay and one transaction. The deploy script
  refuses to run without `PIRON_UPGRADE_AUTHORITY` and reads the authority back from chain
  after setting it, so the step cannot be silently skipped — but the vault itself has to
  exist first.

## What the backend has to change, not just read

Three things that are not address entries.

**`NetworkConfig.chainId` is a `number`.** Neither Stellar nor Solana has a numeric chain
id — Stellar identifies a network by passphrase, Solana by genesis hash. Adding these two
chains to `src/contracts/chains/` needs the type widened first, not a fake id invented for
them.

**`ContractAddresses` assumes one manager per pool type.** It has a single
`lockedPoolManager`, because on EVM there is one. On Stellar each pool has its own manager
contract, so the per-pool addresses belong on the pool record, not the chain record.

**Error codes are shared between the two Rust chains, deliberately.** Both report `701`
for an unauthorised caller and the same numbers throughout, so one mapping covers Stellar
and Solana. The ones most likely to reach a user:

| Code | Meaning | Where |
| --- | --- | --- |
| 701 | Unauthorised caller | every contract, both chains |
| 715 | `AlreadyBound` — upgrader already claimed | any upgradeable contract |
| 717 | `PoolEscrowNotSet` — no loan destination registered | yield-reserve |
| 718 | `WrongRecipient` — loan aimed somewhere other than the pool's escrow | yield-reserve |
| 719 | `NothingToSync` on the reserve; `TimelockNotSet` on the factory | both |

719 is reused across two contracts. They cannot be confused in practice — one comes from
`sync_untracked`, the other from a pool creation — but the mapping has to key on the
contract as well as the code, not on the code alone.

## Two things that are true and may look like bugs

Recorded so nobody spends a day rediscovering them.

**No Soroban manager ever calls the fee manager.** All three release fees directly to
`roles.treasury` through their escrow — `stable_yield_manager:564`,
`single_asset_manager:701`, `locked_pool_manager:535`. So the fee manager's
treasury/reserve/ops split does not run on Stellar: the whole fee lands at the treasury,
and the yield reserve's only funding is `receive_yield` from a registered source. On EVM
the reserve is partly funded by that split. Fees are not lost, they are routed differently,
and closing the gap is a protocol change rather than a configuration one. Not done.

**The access-manager deployment window is still open** — `deployment_complete` is `false`,
so the admin can still grant any role instantly. That is deliberate for now: a locked pool
created after `finalize_deployment` needs Operator, and after finalisation that is a
24-hour propose-then-execute by the multisig. Close the window once the pools a deployment
needs exist. `create_soroban_pool.sh` detects the closed window and falls back to proposing
rather than failing.
