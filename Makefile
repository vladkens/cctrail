.PHONY: prepare check test build update clean

CARGO_FLAGS := --release --locked

prepare:
	cargo +nightly fmt
	cargo clippy $(CARGO_FLAGS) --fix --all-targets --allow-dirty --allow-no-vcs -- -D warnings
	cargo check $(CARGO_FLAGS)

check:
	cargo +nightly fmt --check
	cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings
	cargo check $(CARGO_FLAGS)

test:
	cargo test $(CARGO_FLAGS)

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/cctrail

update: # dependencies, then the bundled prices from a models.dev checkout in 3rd/
	cargo upgrade -i
	uv run scripts/pricing-history.py

clean:
	cargo clean

.PHONY: compare bench

compare: build # the numbers against a reference build on the local history: REF=<main's binary>
	uv run scripts/compare.py --ref "$(REF)"

bench: build # warm runs over the full local history: reads all of it, only for speed work
	CCTRAIL_NO_UPDATE=1 hyperfine --warmup 1 --runs 10 --output null \
		'target/release/cctrail --by dir --json'
