.PHONY: build test lint fmt fmt-check bench check-deps ci size clean

build:
	cargo build --workspace

test:
	cargo test --workspace

lint:
	cargo clippy --workspace -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

bench:
	cargo bench

check-deps:
	D_LINES=$$(cargo tree -p domain 2>&1 | wc -l | tr -d ' '); \
	if [ "$$D_LINES" -eq 1 ]; then echo "domain pure OK"; else echo "DOMAIN IMPURE"; cargo tree -p domain; exit 1; fi

size:
	ls -la target/release/ag

ci: fmt-check lint test build

clean:
	cargo clean
