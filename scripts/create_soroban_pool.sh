#!/usr/bin/env bash
#
# Creates one pool through the factory, as an admin would.
#
#   ./scripts/create_soroban_pool.sh single-asset  [network]
#   ./scripts/create_soroban_pool.sh locked        [network]
#   ./scripts/create_soroban_pool.sh stable-yield  [network]
#
# Separate from the deploy script on purpose. A pool has a term, a target raise, an asset
# and a share name — choices that belong to whoever is opening it, not to the script that
# stands the system up. EVM works the same way: `DeployUpgradeable.s.sol` deploys the
# system, and pools are created afterwards through `PoolFactory`.
#
# Parameters below are testnet defaults, overridable by environment variable. They are
# there so the script runs unattended for integration work, not because they are the right
# numbers for a real pool.
#
# Appends to deployments/soroban-<network>-pools.env. Appends rather than overwrites:
# there will be many pools, and the previous one is not superseded by the next.

set -euo pipefail

KIND="${1:-}"
NETWORK="${2:-testnet}"
SOURCE="${SOURCE:-piron-deployer}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INFRA="$ROOT/deployments/soroban-$NETWORK.env"
OUT="$ROOT/deployments/soroban-$NETWORK-pools.env"

case "$KIND" in
  single-asset|locked|stable-yield) ;;
  *) echo "usage: $0 <single-asset|locked|stable-yield> [network]" >&2; exit 2 ;;
esac

if [[ ! -f "$INFRA" ]]; then
  echo "No infrastructure for $NETWORK. Run ./scripts/deploy_soroban.sh $NETWORK first." >&2
  exit 1
fi
# shellcheck disable=SC1090
set -a; source "$INFRA"; set +a

call() { local id="$1"; shift
  local out
  if ! out="$(stellar contract invoke --id "$id" --source-account "$SOURCE" \
      --network "$NETWORK" -- "$@" 2>&1)"; then
    echo "FAILED: invoke $id $*" >&2
    echo "$out" | tail -5 >&2
    return 1
  fi
  echo "$out" | tail -1; }

# A locked pool has no share token — its positions each have their own maturity, so there
# is nothing to make fungible — and the factory returns `share: null` for one.
field() { python3 -c 'import json,sys; v=json.load(sys.stdin)[sys.argv[1]]; print("" if v is None else v)' "$1"; }

ADMIN="$PIRON_ADMIN"
ASSET="$PIRON_ASSET"
FACTORY="$PIRON_POOL_FACTORY"
TIMELOCK="$PIRON_UPGRADE_TIMELOCK"
RESERVE_C="$PIRON_YIELD_RESERVE"

# The factory refuses to build a pool whose upgrade slot would be left open, so check the
# cause here rather than reading #719 out of a trapped transaction.
CONFIGURED="$(call "$FACTORY" timelock | tr -d '"')"
if [[ "$CONFIGURED" != "$TIMELOCK" ]]; then
  echo "FAILED: factory has no upgrade authority configured ('$CONFIGURED')." >&2
  echo "Run: stellar contract invoke --id $FACTORY -- set_timelock --caller $ADMIN --timelock $TIMELOCK" >&2
  exit 1
fi

NOW="$(date +%s)"
SALT="$(openssl rand -hex 32)"
# `upgrader` must be present for the CLI to parse the Roles type, even though the factory
# overwrites it with the timelock it was configured with — a pool creator does not get to
# name the authority that can replace its bytecode.
ROLES="{\"admin\":\"$ADMIN\",\"operator\":\"$PIRON_OPERATOR\",\"spv\":\"$PIRON_SPV\",\"emergency\":\"$PIRON_EMERGENCY\",\"treasury\":\"$PIRON_TREASURY\",\"upgrader\":\"$TIMELOCK\"}"

echo "Creating a $KIND pool on $NETWORK..."
PREDICTED="$(call "$FACTORY" predict --salt "$SALT")"

case "$KIND" in

single-asset)
  # One instrument, one term. Shares are priced off a divisor frozen at maturity, so the
  # term and the raise are the pool, not parameters of it.
  NAME="${POOL_NAME:-Piron 90d Bill}"
  SYMBOL="${POOL_SYMBOL:-pBILL90}"
  TARGET="${POOL_TARGET_RAISE:-1000000000}"
  CONFIG="{\"instrument_type\":0,\"face_value\":\"0\",\"purchase_price\":\"$TARGET\",\"target_raise\":\"$TARGET\",\"epoch_end_time\":$((NOW+604800)),\"maturity_date\":$((NOW+7776000)),\"coupon_dates\":[],\"coupon_rates\":[],\"discount_rate\":${POOL_DISCOUNT_BPS:-500},\"minimum_funding_threshold\":8000,\"min_investment\":\"${POOL_MIN_INVESTMENT:-10000000}\",\"withdrawal_fee_bps\":${POOL_WITHDRAWAL_FEE_BPS:-50}}"
  META="{\"name\":\"$NAME\",\"symbol\":\"$SYMBOL\",\"decimals\":7}"
  POOL="$(call "$FACTORY" create_single_asset_pool --caller "$ADMIN" --salt "$SALT" \
    --config "$CONFIG" --roles "$ROLES" --asset "$ASSET" --share_metadata "$META")"
  ;;

locked)
  # Early exit pays principal plus interest accrued to today, and the escrow holds only
  # the principal until the SPV returns yield — so the exit is short by the interest
  # earned, in the ordinary case rather than an edge one. The reserve lends the
  # difference, which is what the three wiring calls below are for.
  NAME="${POOL_NAME:-Piron Locked}"
  POOL="$(call "$FACTORY" create_locked_pool --caller "$ADMIN" --salt "$SALT" \
    --roles "$ROLES" --asset "$ASSET" \
    --min_investment "${POOL_MIN_INVESTMENT:-10000000}" --reserve "\"$RESERVE_C\"")"
  ;;

stable-yield)
  # Rolling and NAV-priced: no term, no target raise. `holding_period` is how long a
  # depositor must stay in before they may queue to leave, and the reserve floor is what
  # keeps the pool from deploying itself empty.
  NAME="${POOL_NAME:-Piron Stable Yield}"
  SYMBOL="${POOL_SYMBOL:-pSY}"
  CONFIG="{\"min_investment\":\"${POOL_MIN_INVESTMENT:-10000000}\",\"holding_period\":${POOL_HOLDING_PERIOD:-604800},\"withdrawal_fee_bps\":${POOL_WITHDRAWAL_FEE_BPS:-50},\"min_absolute_reserve\":\"${POOL_MIN_RESERVE:-100000000}\",\"reserve_ratio_bps\":${POOL_RESERVE_RATIO_BPS:-1000},\"is_active\":true}"
  META="{\"name\":\"$NAME\",\"symbol\":\"$SYMBOL\",\"decimals\":7}"
  POOL="$(call "$FACTORY" create_stable_yield_pool --caller "$ADMIN" --salt "$SALT" \
    --roles "$ROLES" --asset "$ASSET" --config "$CONFIG" --share_metadata "$META")"
  ;;

esac

MANAGER="$(echo "$POOL" | field manager)"
ESCROW="$(echo "$POOL" | field escrow)"
SHARE="$(echo "$POOL" | field share)"

# ------------------------------------------------------------ per-pool wiring
#
# Only the locked pool needs any. A single-asset or stable-yield pool is self-contained:
# its escrow holds the money, its share token is minted by it alone, and fees are released
# to `roles.treasury` by the manager itself.
#
# Deliberately not done, and both were in the old deploy script:
#
#   - Registering the manager as a fee-manager collector. No Soroban manager holds a
#     fee-manager client — all three release fees straight to `roles.treasury` — so the
#     registration authorises a call that is never made.
#   - Registering the manager as a reserve source. `settle_debt` forwards `repay_loan`
#     with `from` set to the SPV that called it, not to the manager, so it is the SPV's
#     registration that matters and the deploy script already makes it protocol-wide.

if [[ "$KIND" == "locked" ]]; then
  echo "Wiring the locked pool to the yield reserve..."

  # 1. The manager calls `loan_to_pool`, which requires Operator.
  if [[ "$(call "$PIRON_ACCESS_MANAGER" deployment_complete)" == "true" ]]; then
    echo "  The instant-grant window is closed, so Operator goes through governance."
    PROPOSAL="$(call "$PIRON_ACCESS_MANAGER" propose_role_grant \
      --caller "$ADMIN" --role 2 --account "$MANAGER")"
    echo "  Proposal $PROPOSAL — executable in 24h by the multisig ($PIRON_MULTISIG):"
    echo "    stellar contract invoke --id $PIRON_ACCESS_MANAGER --source-account <multisig> \\"
    echo "      --network $NETWORK -- execute_role_grant --caller $PIRON_MULTISIG --proposal_id $PROPOSAL"
    echo "  Until it executes, an early exit the escrow cannot cover will fail."
  else
    call "$PIRON_ACCESS_MANAGER" grant_during_deployment \
      --caller "$ADMIN" --role 2 --account "$MANAGER" >/dev/null
    echo "  Operator granted to the manager"
  fi

  # 2. The reserve pays loans only to the escrow registered for the pool being charged
  #    (`require_pool_escrow`). Without this, every loan fails with PoolEscrowNotSet — and
  #    the loan path is the ordinary early-exit path, not an edge one.
  call "$RESERVE_C" set_pool_escrow --caller "$ADMIN" --pool "$MANAGER" --escrow "$ESCROW" >/dev/null
  echo "  Loan destination bound to its own escrow"

  BOUND_ESCROW="$(call "$RESERVE_C" pool_escrow --pool "$MANAGER" | tr -d '"')"
  if [[ "$BOUND_ESCROW" != "$ESCROW" ]]; then
    echo "FAILED: reserve records '$BOUND_ESCROW' for this pool, expected '$ESCROW'" >&2
    exit 1
  fi
fi

# The manager bound its upgrader in its own constructor, from the timelock the factory
# stamped on it. A pool whose upgrade slot was somehow left open is the thing worth
# catching, because on Soroban that slot is claimed once and never released.
BOUND="$(call "$MANAGER" upgrader | tr -d '"')"
if [[ "$BOUND" != "$TIMELOCK" ]]; then
  echo "FAILED: $MANAGER upgrader is '$BOUND', expected the timelock '$TIMELOCK'" >&2
  exit 1
fi

mkdir -p "$ROOT/deployments"
if [[ ! -f "$OUT" ]]; then
  cat > "$OUT" <<EOF
# Piron pools on Stellar — $NETWORK
# Appended to by scripts/create_soroban_pool.sh. One block per pool.
EOF
fi
PREFIX="PIRON_$(echo "$KIND" | tr 'a-z-' 'A-Z_')_$(call "$FACTORY" pool_count)"
cat >> "$OUT" <<EOF

# $NAME — $KIND, created $(date -u +%Y-%m-%dT%H:%M:%SZ)
${PREFIX}_MANAGER=$MANAGER
${PREFIX}_ESCROW=$ESCROW
${PREFIX}_SHARE=$SHARE
EOF

echo
echo "Predicted before creation: $PREDICTED"
echo "Created:"
echo "  manager  $MANAGER"
echo "  escrow   $ESCROW"
echo "  share    $SHARE"
echo "  upgrader $BOUND (the timelock)"
echo
echo "Recorded in $OUT"
