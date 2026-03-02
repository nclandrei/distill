.PHONY: build test lint fmt check clean

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

clean:
	cargo clean
