.PHONY: build test lint fmt check local-checks hooks-install clean perf-check perf-benchmark

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

perf-benchmark: build
	@mkdir -p .distill-runtime/perf
	./scripts/benchmark_scan.sh --report .distill-runtime/perf/scan-report.json

perf-check: build
	@mkdir -p .distill-runtime/perf
	./scripts/benchmark_scan.sh --report .distill-runtime/perf/scan-report.json
	./scripts/check_scan_perf.py --budget perf/scan-budget.json .distill-runtime/perf/scan-report.json

hooks-install:
	./scripts/install-hooks.sh

clean:
	cargo clean
