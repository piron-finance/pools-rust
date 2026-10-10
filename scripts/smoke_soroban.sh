#!/usr/bin/env bash
#
# Deposits into a live single-asset pool and checks the money arrived.
#
#   ./scripts/smoke_soroban.sh testnet
#
# Run after every deploy. A deploy script that exits 0 proves the calls it makes work, not
# that a user can do anything — and the calls a deploy script makes are all administrative.
# This one moves an investor's money, which is the only flow that matters.
#
# Creates its own single-asset pool through the live factory and deposits into that, so
# the test is repeatable. Depositing into a shared pool is not: a single-asset pool has a
# target raise, and the second run hits ExceedsTargetRaise (#203) through no fault of the
# deployment. Creating the pool also exercises the factory, which is the other half of
# what the backend depends on.

set -euo pipefail

NETWORK="${1:-testnet}"
SOURCE="${SOURCE:-piron-deployer}"
INVESTOR="${INVESTOR:-piron-smoke-investor}"
AMOUNT="${AMOUNT:-600000000}"   # 60 XLM at 7dp, inside the demo pool's 100 XLM target
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

INFRA="$ROOT/deployments/soroban-$NETWORK.env"
[[ -f "$INFRA" ]] || { echo "Missing $INFRA — run ./scripts/deploy_soroban.sh $NETWORK." >&2; exit 1; }
# shellcheck disable=SC1090
set -a; source "$INFRA"; set +a

echo "Creating a pool to deposit into..."
POOL_NAME="Piron Smoke $(date -u +%H%M%S)" POOL_SYMBOL=pSMOKE \
POOL_TARGET_RAISE=10000000000 POOL_MIN_INVESTMENT=10000000 \
  "$ROOT/scripts/create_soroban_pool.sh" single-asset "$NETWORK" >/dev/null

# The run above appended its block, and its manager is the newest one in the file.
POOLS="$ROOT/deployments/soroban-$NETWORK-pools.env"
# shellcheck disable=SC1090
set -a; source "$POOLS"; set +a
LAST="$(grep -o '^PIRON_SINGLE_ASSET_[0-9]*_' "$POOLS" | tail -1)"
MGR="$(eval echo "\$${LAST}MANAGER")"
ESC="$(eval echo "\$${LAST}ESCROW")"
SHR="$(eval echo "\$${LAST}SHARE")"
[[ -n "$MGR" ]] || { echo "Could not read back the pool just created." >&2; exit 1; }

inv() { local id="$1" acct="$2"; shift 2
  stellar contract invoke --id "$id" --source-account "$acct" --network "$NETWORK" \
    "$@" 2>&1 | tail -1; }

stellar keys generate "$INVESTOR" --network "$NETWORK" >/dev/null 2>&1 || true
stellar keys fund "$INVESTOR" --network "$NETWORK" >/dev/null 2>&1 || true
INV="$(stellar keys address "$INVESTOR")"

echo "Pool    $MGR"
echo "Escrow  $ESC"
echo "Asset   $PIRON_ASSET"
echo

num() { tr -d '"'; }
ESC_BEFORE="$(inv "$ESC" "$SOURCE" -- balance | num)"
SUP_BEFORE="$(inv "$SHR" "$SOURCE" -- total_supply | num)"
printf "before  escrow=%s  share_supply=%s\n" "$ESC_BEFORE" "$SUP_BEFORE"

echo "Depositing $AMOUNT as $INV..."
if ! out="$(stellar contract invoke --id "$MGR" --source-account "$INVESTOR" \
    --network "$NETWORK" --send=yes -- deposit --investor "$INV" --assets "$AMOUNT" 2>&1)"; then
  echo "FAILED: the deposit did not land." >&2
  echo "$out" | grep -iE "error|Error" | head -4 >&2
  exit 1
fi

ESC_AFTER="$(inv "$ESC" "$SOURCE" -- balance | num)"
SUP_AFTER="$(inv "$SHR" "$SOURCE" -- total_supply | num)"
INV_SHARES="$(inv "$SHR" "$SOURCE" -- balance --id "$INV" | num)"
printf "after   escrow=%s  share_supply=%s  investor_shares=%s\n" \
  "$ESC_AFTER" "$SUP_AFTER" "$INV_SHARES"

fail=0
check() { local what="$1" got="$2" want="$3"
  if [[ "$got" == "$want" ]]; then printf "  OK   %s = %s\n" "$what" "$got"
  else printf "  WRONG %s = %s, expected %s\n" "$what" "$got" "$want"; fail=1; fi; }

check "escrow grew by the deposit"      "$((ESC_AFTER - ESC_BEFORE))" "$AMOUNT"
check "share supply grew by the deposit" "$((SUP_AFTER - SUP_BEFORE))" "$AMOUNT"
check "the investor holds those shares"  "$INV_SHARES"                 "$AMOUNT"

[[ $fail -eq 0 ]] || { echo "The deposit landed but the accounting does not agree." >&2; exit 1; }
echo
echo "Money moved and the books agree. $NETWORK is usable."
