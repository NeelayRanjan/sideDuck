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
	$(CARGO) run --release --locked -p sideduck-brush --example brush_gate
	./target/release/sideduck-bench --seconds 2 --root "$(GATE_ROOT)"
	./target/release/render-gate --root "$(GATE_ROOT)" --results target/render-results.json
	./target/release/render-gate --fit --root "$(GATE_ROOT)" --results target/fit-results.json
test:
	$(CARGO) test --workspace --locked
coverage:
	./scripts/coverage.sh
bench:
	$(CARGO) run --release --locked -p sideduck-brush --example brush_gate
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 2 --root "$(GATE_ROOT)"
	$(CARGO) run --release --locked -p sideduck-bench --bin render-gate -- --root "$(GATE_ROOT)" --results target/render-results.json
	$(CARGO) run --release --locked -p sideduck-bench --bin render-gate -- --fit --root "$(GATE_ROOT)" --results target/fit-results.json
soak:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 600 --root "$(GATE_ROOT)" --results target/soak-results.json
run:
	$(CARGO) run --release --locked -p sideduck-app
dist:
	./scripts/dist.sh

