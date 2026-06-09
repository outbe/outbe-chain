.DEFAULT_GOAL := help

CARGO ?= cargo
NEXTEST ?= $(CARGO) nextest run
OUT_DIR ?= /tmp/outbe-testnet
VALIDATORS ?= 4
SEED_FILE ?=
OUTBE_CHAIN_BINARY ?= ./target/release/outbe-chain

.PHONY: help build build-release audit audit-release test test-doc test-consensus nextest-install audit-tools-install audit-quick audit-full audit-deny audit-rustsec audit-machete audit-udeps audit-miri audit-geiger audit-bloat audit-llvm-lines audit-coverage audit-bench localnet-bootstrap localnet-start localnet-stop localnet-status localnet localnet-restart localnet-clean localnet-chain244-smoke sgx-smoke bench-tribute bench-tribute-sgx

help:
	@printf '%s\n' \
		'Available targets:' \
		'  make build                Build the workspace (debug)' \
		'  make build-release        Build the workspace (release)' \
		'  make audit                Audit debug binaries for known vulnerabilities' \
		'  make audit-release        Audit release binaries for known vulnerabilities' \
		'  make test                 Run the default test suite (cargo nextest run + cargo test --doc)' \
		'  make test-doc             Run workspace doctests only (nextest skips doctests)' \
		'  make test-consensus       Run consensus crate tests via nextest' \
		'  make nextest-install      Install cargo-nextest locally (one-shot helper)' \
		'' \
		'Audit toolchain (rust-module-audit skill entry points):' \
		'  make audit-tools-install  Install full cargo audit toolchain + nightly + miri/llvm components' \
		'  make audit-quick          Fast cycle: clippy -D warnings + cargo nextest run' \
		'  make audit-full           Full cycle: machete + deny + rustsec + udeps' \
		'  make audit-deny           cargo deny check (licenses, advisories, bans, sources)' \
		'  make audit-rustsec        cargo audit (RustSec workspace dependency advisories)' \
		'  make audit-machete        cargo machete (unused workspace dependencies)' \
		'  make audit-udeps          cargo +nightly udeps --workspace --all-targets' \
		'  make audit-miri           cargo +nightly miri test --lib (UB / unsafe surface)' \
		'  make audit-geiger         cargo geiger (unsafe surface count per crate)' \
		'  make audit-bloat          cargo bloat --release --crates -n 30' \
		'  make audit-llvm-lines     cargo llvm-lines --release | head -50' \
		'  make audit-coverage       cargo llvm-cov nextest --html (HTML coverage report)' \
		'  make audit-bench          cargo bench (criterion baseline; configure benches per crate)' \
		'' \
		'  make localnet-bootstrap   Clean OUT_DIR and bootstrap a local validator set' \
		'  make localnet-start       Start a bootstrapped localnet from OUT_DIR' \
		'  make localnet-stop        Stop the localnet in OUT_DIR' \
		'  make localnet-status      Show localnet process status for OUT_DIR' \
		'  make localnet             Build release + bootstrap + start localnet' \
		'  make localnet-restart     Stop and start the bootstrapped localnet' \
		'  make localnet-clean       Remove all validator-*/ and pids/ from OUT_DIR' \
		'  make localnet-chain244-smoke  Build + run system-tx localnet smoke' \
		'' \
		'Variables:' \
		'  CARGO=cargo' \
		'  NEXTEST="$$(CARGO) nextest run"' \
		'  OUT_DIR=/tmp/outbe-testnet' \
		'  VALIDATORS=4' \
		'  SEED_FILE=scripts/seed-testnet.json' \
		'  OUTBE_CHAIN_BINARY=./target/release/outbe-chain'

build:
	$(CARGO) auditable build

build-release:
	$(CARGO) auditable build --release

audit: build
	find target/debug -maxdepth 1 -type f -perm +111 -name 'outbe-*' -exec $(CARGO) audit bin {} \;

audit-release: build-release
	find target/release -maxdepth 1 -type f -perm +111 -name 'outbe-*' -exec $(CARGO) audit bin {} \;

test:
	$(NEXTEST) --workspace
	$(CARGO) test --doc --workspace

test-doc:
	$(CARGO) test --doc --workspace

test-consensus:
	$(NEXTEST) -p outbe-consensus

nextest-install:
	$(CARGO) install --locked cargo-nextest

# --- Audit toolchain (used by .ruler/skills/rust-module-audit) ---

audit-tools-install:
	$(CARGO) install --locked cargo-nextest cargo-machete cargo-deny cargo-audit cargo-llvm-cov cargo-llvm-lines cargo-bloat
	$(CARGO) install --locked cargo-udeps
	$(CARGO) install cargo-geiger
	rustup toolchain install nightly
	rustup +nightly component add miri rust-src
	rustup component add llvm-tools-preview clippy rustfmt

audit-quick:
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(NEXTEST) --workspace

audit-full: audit-machete audit-deny audit-rustsec audit-udeps

audit-deny:
	$(CARGO) deny check

audit-rustsec:
	$(CARGO) audit

audit-machete:
	$(CARGO) machete

audit-udeps:
	$(CARGO) +nightly udeps --workspace --all-targets

audit-miri:
	$(CARGO) +nightly miri test --lib

audit-geiger:
	$(CARGO) geiger

audit-bloat:
	$(CARGO) bloat --release --crates -n 30

audit-llvm-lines:
	$(CARGO) llvm-lines --release | head -50

audit-coverage:
	$(CARGO) llvm-cov nextest --workspace --html

audit-bench:
	$(CARGO) bench --workspace

localnet-bootstrap:
ifeq ($(strip $(SEED_FILE)),)
	OUTBE_CHAIN_BINARY=$(OUTBE_CHAIN_BINARY) ./scripts/bootstrap-testnet.sh $(VALIDATORS) $(OUT_DIR)
else
	OUTBE_CHAIN_BINARY=$(OUTBE_CHAIN_BINARY) ./scripts/bootstrap-testnet.sh $(VALIDATORS) $(OUT_DIR) $(SEED_FILE)
endif

localnet-start:
	OUTBE_CHAIN_BINARY=$(OUTBE_CHAIN_BINARY) ./scripts/run-testnet.sh start $(OUT_DIR)
	OUTBE_CHAIN_BINARY=$(OUTBE_CHAIN_BINARY) ./scripts/run-testnet.sh status $(OUT_DIR)

localnet-stop:
	./scripts/run-testnet.sh stop $(OUT_DIR)

localnet-status:
	OUTBE_CHAIN_BINARY=$(OUTBE_CHAIN_BINARY) ./scripts/run-testnet.sh status $(OUT_DIR)

localnet: build-release localnet-bootstrap localnet-start

localnet-restart: localnet-stop localnet-start

localnet-clean:
	./scripts/clean-testnet-data.sh $(OUT_DIR)

localnet-chain244-smoke:
	OUTBE_CHAIN_BINARY=$${OUTBE_CHAIN_BINARY:-./target/debug/outbe-chain} VALIDATORS=$(VALIDATORS) ./scripts/chain244-system-tx-smoke.sh $(OUT_DIR)

# Hardware SGX smoke: build + sign + run the enclave under gramine-sgx and assert
# real SGX execution + EGETKEY sealing (the "≥1 hw SGX smoke" acceptance check).
# Requires gramine + SGX hardware; no-op skip on non-SGX boxes.
sgx-smoke:
	./scripts/sgx-smoke.sh

# Tribute-offer enclave throughput. Two layers:
#  1) criterion micro-bench: pure in-enclave CPU cost per offer (decrypt +
#     economics + Poseidon token_id) -> the offers/sec ceiling;
#  2) e2e test: same path INCLUDING the Noise/UDS transport round-trip.
# Both run native. For the production SGX figure, run the e2e under gramine-sgx.
bench-tribute:
	cargo bench -p outbe-tee-enclave --bench tribute_offer_throughput
	cargo test -p outbe-tee-enclave --release --test transport \
		transport_throughput_offers_per_sec -- --ignored --nocapture

# Production throughput figure: drive offers through the enclave under REAL
# gramine-sgx (SGX enter/exit + syscall emulation + Noise-IK included).
# Requires gramine + SGX hardware; SKIPs on non-SGX boxes.
bench-tribute-sgx:
	./scripts/sgx-bench.sh
