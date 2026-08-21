# Piron — Rust implementations for Stellar and Solana.
#
# The Solidity reference lives in its own repo. `make parity` reads it; nothing here
# ever writes to it.

.PHONY: help test build deploy parity parity-accept fmt clippy

help:
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "};{printf "  %-16s %s\n", $$1, $$2}'

test:              ## Run every test: shared logic and contracts
	@cargo build --target wasm32v1-none --release --quiet
	@cargo test

build:             ## Build the Soroban contracts to wasm
	@cargo build --target wasm32v1-none --release
	@ls -la target/wasm32v1-none/release/*.wasm

deploy:            ## Deploy and wire the contracts (NETWORK=testnet)
	@./scripts/deploy_soroban.sh $(or $(NETWORK),testnet)

parity:            ## Has the Solidity moved without us?
	@python3 parity/check.py
	@python3 parity/sync_errors.py --check

parity-accept:     ## Re-baseline after reflecting Solidity changes
	@python3 parity/check.py --accept
	@python3 parity/sync_errors.py

fmt:               ## Format
	@cargo fmt

clippy:            ## Lint
	@cargo clippy --all-targets -- -D warnings
