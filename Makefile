.PHONY: build test lint fmt fmt-check bench check-deps check-arch secrets-scan ci size clean

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
	ls -la target/release/zcode

check-arch:
	bash docs/architecture/dependency-check.sh

# M2.6: no API keys committed. Config references secrets by env-var *name*.
secrets-scan:
	@! grep -rInE '(api[_-]?key|secret|token)[[:space:]]*=[[:space:]]*"[A-Za-z0-9_-]{16,}"' \
		--include='*.rs' --include='*.toml' --include='*.md' \
		crates benches docs *.toml 2>/dev/null || (echo "possible committed secret"; exit 1)
	@echo "secrets-scan OK"

ci: fmt-check lint test build check-deps secrets-scan

clean:
	cargo clean
