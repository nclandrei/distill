.PHONY: build test lint fmt check local-checks hooks-install clean

build:
	cargo build

test:
	cargo test

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

check: fmt lint test
	@echo "All checks passed."

local-checks:
	./scripts/local-checks.sh

hooks-install:
	./scripts/install-hooks.sh

clean:
	cargo clean
