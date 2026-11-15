# TerminalBench developer entry points. The product itself is the Rust
# binary (`make build` -> target/release/tbench, or just use bin/tbench,
# which builds on demand); everything else here is development hygiene.

CARGO ?= cargo
BIN   := target/release/tbench

.DEFAULT_GOAL := help

.PHONY: help build debug test unit conformance lint fmt clippy check run clean install

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-14s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Release build (target/release/tbench)
	$(CARGO) build --release

debug: ## Debug build
	$(CARGO) build

test: unit conformance ## All tests (Rust unit tests + Reporting Spec v1 suite)

unit: ## Rust unit and integration tests (TEST_ARGS="-- --nocapture" shows what the pty tests observed)
	$(CARGO) test $(TEST_ARGS)

conformance: build ## Reporting Spec v1 regression suite (tests/report-v1.sh)
	zsh tests/report-v1.sh

lint: fmt-check clippy ## rustfmt --check + clippy (deny warnings)

fmt: ## Format Rust sources in place
	$(CARGO) fmt

.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

check: lint test ## Everything CI would run: lint + all tests

run: build ## Run the full benchmark suite (all scenarios, default shells)
	./$(BIN) run

clean: ## Remove build artifacts (keeps results/)
	$(CARGO) clean

install: ## Install the tbench binary via cargo install (works from any directory)
	$(CARGO) install --path .
