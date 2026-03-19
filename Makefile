NAME       := tm
TARGET     := $(shell rustc -vV | awk '/^host:/ {print $$2}')
LLVM_BIN   := $(shell rustc --print sysroot)/lib/rustlib/$(TARGET)/bin
PGO_DIR    := $(CURDIR)/target/pgo-profiles
PGO_MERGED := $(PGO_DIR)/merged.profdata

.PHONY: build release release-pgo pgo-profile bench bench-pgo install test

build:
	cargo build

release:
	cargo build --release

# Collect PGO profiles from benchmarks.
# No -Cpanic=immediate-abort: the profiler runtime needs unwinding.
pgo-profile:
	rm -rf $(PGO_DIR) && mkdir -p $(PGO_DIR)
	RUSTFLAGS="-Cprofile-generate=$(PGO_DIR)" \
	cargo bench --bench benchmarks -- --profile-time 1
	$(LLVM_BIN)/llvm-profdata merge -o $(PGO_MERGED) $(PGO_DIR)

# PGO-optimized release build.
release-pgo: $(PGO_MERGED)
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	cargo build --release --target $(TARGET)

# Benchmark regular release vs PGO. Requires: critcmp (cargo install critcmp)
bench-pgo: $(PGO_MERGED)
	cargo bench --bench benchmarks -- --save-baseline regular 2>/dev/null
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	cargo bench --bench benchmarks -- --save-baseline pgo 2>/dev/null
	critcmp regular pgo

$(PGO_MERGED):
	$(MAKE) pgo-profile

bench:
	cargo bench

test:
	@OUT=$$(cargo test --bin tm --quiet -- --test-threads=32 2>&1) || { echo "$$OUT"; exit 1; }

install: release-pgo
	cp target/$(TARGET)/release/$(NAME) ~/usr/bin/$(NAME)

setup:
	prek install --install-hooks

pc:
	prek --quiet run --all-files
