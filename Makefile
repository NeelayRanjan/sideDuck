CARGO := ./scripts/cargo.sh
GATE_ROOT ?= target/storage-gate
.PHONY: setup check test coverage bench soak run dist
setup:
	$(CARGO) fetch --locked
	$(CARGO) install cargo-llvm-cov --version 0.6.21 --locked
check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) test --workspace --locked
	./scripts/coverage.sh
	$(CARGO) build --workspace --release --locked
	./target/release/sideduck-bench --seconds 2 --root "$(GATE_ROOT)"
test:
	$(CARGO) test --workspace --locked
coverage:
	./scripts/coverage.sh
bench:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 2 --root "$(GATE_ROOT)"
soak:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 600 --root "$(GATE_ROOT)" --results target/soak-results.json
run:
	$(CARGO) run --release --locked -p sideduck-app
dist:
	./scripts/dist.sh

