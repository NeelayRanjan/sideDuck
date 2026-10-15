CARGO := ./scripts/cargo.sh
.PHONY: setup check test bench soak run dist
setup:
	$(CARGO) fetch --locked
check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) test --workspace --locked
	$(CARGO) build --workspace --release --locked
test:
	$(CARGO) test --workspace --locked
bench:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 2
soak:
	$(CARGO) run --release --locked -p sideduck-bench -- --seconds 600
run:
	$(CARGO) run --release --locked -p sideduck-app
dist:
	./scripts/dist.sh

