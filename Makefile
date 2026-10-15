CARGO := ./scripts/cargo.sh
GATE_ROOT ?= target/storage-gate
.PHONY: setup check test bench soak run dist
setup:
	$(CARGO) fetch --locked
check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) test --workspace --locked
	$(CARGO) build --workspace --release --locked
	./target/release/sideduck-bench --seconds 2 --root "$(GATE_ROOT)"
test:
	$(CARGO) test --workspace --locked
bench:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 2 --root "$(GATE_ROOT)"
soak:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 600 --root "$(GATE_ROOT)" --results target/soak-results.json
run:
	$(CARGO) run --release --locked -p sideduck-app
dist:
	./scripts/dist.sh

