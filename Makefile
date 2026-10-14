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

.PHONY: dist compare bench

# Prebuilt binaries as target/distrib/cctrail-dev-<target>.tar.gz (.zip for Windows), from an Apple
# Silicon Mac: Linux (glibc 2.28) and Windows through cargo-cross, installed at a fixed version,
# with its toolchains kept in ~/.cache/cargo-cross (CI caches that folder).
# macOS keeps its file and line info in cctrail.dSYM, which has to stay next to the binary. Not
# target/dist: cargo keeps the dist profile's build scripts there.
dist: T := $(or $(CARGO_TARGET_DIR),target)
dist: # the release archives for CI (vladkens/actions/rust-build)
	cargo install --locked cargo-cross@1.6.0
	CARGO_PROFILE_DIST_SPLIT_DEBUGINFO=packed cargo build --profile dist --locked --target aarch64-apple-darwin
	CROSS_COMPILER_DIR=$${CROSS_COMPILER_DIR:-$$HOME/.cache/cargo-cross} \
		cargo cross build --profile dist --locked --glibc-version 2.28 \
		--targets x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu,x86_64-pc-windows-gnu
	rm -rf $(T)/distrib && mkdir -p $(T)/distrib
	tar czhf $(T)/distrib/cctrail-dev-aarch64-apple-darwin.tar.gz readme.md LICENSE \
		-C $(T)/aarch64-apple-darwin/dist cctrail cctrail.dSYM
	for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do \
		tar czf $(T)/distrib/cctrail-dev-$$t.tar.gz readme.md LICENSE -C $(T)/$$t/dist cctrail || exit 1; done
	zip -jq $(T)/distrib/cctrail-dev-x86_64-pc-windows-gnu.zip readme.md LICENSE \
		$(T)/x86_64-pc-windows-gnu/dist/cctrail.exe
	ls -lh $(T)/distrib

compare: build # the numbers against a reference build on the local history: REF=<main's binary>
	uv run scripts/compare.py --ref "$(REF)"

bench: build # warm runs over the full local history: reads all of it, only for speed work
	CCTRAIL_NO_UPDATE=1 hyperfine --warmup 1 --runs 10 --output null \
		'target/release/cctrail --by dir --json'
