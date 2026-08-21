#!/usr/bin/env python3
"""Detect Solidity changes that the Rust implementations have not caught up with.

The Solidity lives in its own repository and is the reference implementation. Nothing here
edits it — this only notices when it moves, which is the whole reason the two repos can be
separate without silently drifting apart.

    python3 parity/check.py            # report drift, exit 1 if any ported file moved
    python3 parity/check.py --accept   # re-baseline after reflecting the changes
    python3 parity/check.py --init     # rebuild the manifest from scratch

Each Solidity file carries one of three statuses:

    ported    a Rust counterpart exists — a change here needs reflecting, or explaining
    pending   no Rust counterpart yet — changes are noted, not flagged
    evm-only  no counterpart by design — never flagged

This is deliberately a command you run, not a git hook. While the team is small,
reflecting changes by hand and re-baselining is the whole workflow. When that stops
scaling, the same manifest becomes a CI step and later the index for conformance vectors.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = Path(__file__).resolve().parent / "manifest.json"
CONFIG = Path(__file__).resolve().parent / "config.json"

# The Solidity lives in its own repo. Resolution order, first hit wins:
#   1. --solidity <path>
#   2. $PIRON_SOLIDITY_ROOT
#   3. parity/config.json  {"solidity_root": "..."}
#   4. ../piron-pools/src, the conventional sibling checkout
DEFAULT_SOLIDITY = ROOT.parent / "piron-pools" / "src"


def solidity_root(override: str | None) -> Path:
    if override:
        return Path(override).expanduser().resolve()
    env = os.environ.get("PIRON_SOLIDITY_ROOT")
    if env:
        return Path(env).expanduser().resolve()
    if CONFIG.exists():
        configured = json.loads(CONFIG.read_text()).get("solidity_root")
        if configured:
            path = Path(configured).expanduser()
            return path.resolve() if path.is_absolute() else (ROOT / path).resolve()
    return DEFAULT_SOLIDITY


def solidity_revision(root: Path) -> str:
    """The reference repo's commit, so drift is anchored to a revision rather than a
    working tree someone may have edited."""
    try:
        out = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, check=True,
        )
        return out.stdout.strip()
    except Exception:
        return "unknown"

# Solidity file -> the Rust that mirrors it. Paths are repo-relative.
MIRRORS: dict[str, list[str]] = {
    "src/libraries/CalculationLibrary.sol": [
        "crates/piron-core/src/single_asset/calc.rs",
    ],
    "src/libraries/ValidationLibrary.sol": [
        "crates/piron-core/src/single_asset/validate.rs",
        "crates/piron-core/src/single_asset/flow.rs",  # withdrawal handlers
    ],
    "src/libraries/PoolLifecycleLibrary.sol": [
        "crates/piron-core/src/single_asset/lifecycle.rs",
    ],
    "src/libraries/DepositWithdrawalLibrary.sol": [
        "crates/piron-core/src/single_asset/flow.rs",
    ],
    "src/types/IPoolTypes.sol": [
        "crates/piron-core/src/types.rs",
    ],
    "src/libraries/LockedPoolLibrary.sol": [
        "crates/piron-core/src/locked/calc.rs",
        "crates/piron-core/src/locked/validate.rs",
    ],
    "src/types/ILockedPoolTypes.sol": [
        "crates/piron-core/src/locked/types.rs",
    ],
    "src/libraries/StableYieldNAVLibrary.sol": [
        "crates/piron-core/src/stable_yield/nav.rs",
    ],
    "src/types/IStableYieldTypes.sol": [
        "crates/piron-core/src/stable_yield/types.rs",
    ],
    "src/LockedPoolManager.sol": [
        "crates/piron-core/src/locked/lifecycle.rs",
        "soroban/locked-pool-manager/src/lib.rs",
    ],
    "src/StableYieldManager.sol": [
        "crates/piron-core/src/stable_yield/queue.rs",
        "soroban/stable-yield-manager/src/lib.rs",
    ],
    "src/escrows/PoolEscrow.sol": [
        "soroban/pool-escrow/src/lib.rs",
    ],
    "src/AccessManager.sol": [
        "crates/piron-core/src/governance/roles.rs",
        "soroban/access-manager/src/lib.rs",
    ],
    "src/FeeManager.sol": [
        "crates/piron-core/src/governance/fees.rs",
        "soroban/fee-manager/src/lib.rs",
    ],
    "src/PoolRegistry.sol": [
        "soroban/pool-registry/src/lib.rs",
    ],
    "src/Manager.sol": [
        "soroban/single-asset-manager/src/lib.rs",
        "solana/single-asset/src/lib.rs",
    ],
}

# Files with no Rust counterpart by design.
#
# Interfaces are EVM ABI declarations. Both Rust runtimes generate typed clients from the
# deployed program instead, so there is no hand-written mirror to drift.
EVM_ONLY_PREFIXES = ("src/interfaces/",)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def classify(rel: str) -> str:
    if rel.startswith(EVM_ONLY_PREFIXES):
        return "evm-only"
    if rel in MIRRORS:
        return "ported"
    return "pending"


def scan(root: Path) -> dict[str, dict]:
    if not root.is_dir():
        sys.exit(
            f"Solidity not found at {root}.\n"
            "Point at it with --solidity, $PIRON_SOLIDITY_ROOT, or parity/config.json."
        )
    files = {}
    for path in sorted(root.rglob("*.sol")):
        rel = "src/" + path.relative_to(root).as_posix()
        files[rel] = {
            "sha256": sha256(path),
            "status": classify(rel),
            "mirrors": MIRRORS.get(rel, []),
        }
    return files


def write_manifest(files: dict[str, dict], root: Path) -> None:
    MANIFEST.write_text(
        json.dumps(
            {
                "version": 1,
                "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "reference": "the Solidity repo is the reference implementation",
                "solidity_root": str(root),
                "solidity_revision": solidity_revision(root),
                "files": files,
            },
            indent=2,
        )
        + "\n"
    )


def load_manifest() -> dict[str, dict]:
    if not MANIFEST.exists():
        sys.exit("No manifest. Run: python3 parity/check.py --init")
    return json.loads(MANIFEST.read_text())["files"]


def report(baseline: dict[str, dict], current: dict[str, dict]) -> int:
    changed, added, removed = [], [], []

    for rel, info in current.items():
        if rel not in baseline:
            added.append(rel)
        elif baseline[rel]["sha256"] != info["sha256"]:
            changed.append(rel)
    for rel in baseline:
        if rel not in current:
            removed.append(rel)

    if not (changed or added or removed):
        print(f"In sync — {len(current)} Solidity files match the baseline.")
        return 0

    needs_action = False

    blocking = [r for r in changed if current[r]["status"] == "ported"]
    if blocking:
        needs_action = True
        print("CHANGED — Rust counterparts need review:\n")
        for rel in blocking:
            print(f"  {rel}")
            for mirror in current[rel]["mirrors"]:
                print(f"      -> {mirror}")
            print()

    noted = [r for r in changed if current[r]["status"] == "pending"]
    if noted:
        print("changed, not yet ported (no action needed):")
        for rel in noted:
            print(f"  {rel}")
        print()

    quiet = [r for r in changed if current[r]["status"] == "evm-only"]
    if quiet:
        print(f"changed, EVM-only ({len(quiet)} files, no counterpart by design)\n")

    if added:
        print("NEW Solidity files — decide their status in parity/check.py:")
        for rel in added:
            print(f"  {rel}")
        print()

    if removed:
        needs_action = any(baseline[r]["status"] == "ported" for r in removed)
        print("REMOVED from src/:")
        for rel in removed:
            marker = " (was ported!)" if baseline[r]["status"] == "ported" else ""
            print(f"  {rel}{marker}")
        print()

    print("Reflect the changes in the Rust, then: python3 parity/check.py --accept")
    return 1 if needs_action else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--init", action="store_true", help="build the manifest fresh")
    parser.add_argument("--accept", action="store_true", help="re-baseline to current")
    parser.add_argument("--solidity", help="path to the Solidity repo's src/")
    args = parser.parse_args()

    root = solidity_root(args.solidity)
    current = scan(root)

    if args.init or args.accept:
        write_manifest(current, root)
        verb = "Initialised" if args.init else "Re-baselined"
        counts = {}
        for info in current.values():
            counts[info["status"]] = counts.get(info["status"], 0) + 1
        summary = ", ".join(f"{n} {s}" for s, n in sorted(counts.items()))
        print(f"{verb} — {len(current)} files ({summary}) at {root} @ {solidity_revision(root)}.")
        return 0

    return report(load_manifest(), current)


if __name__ == "__main__":
    raise SystemExit(main())
