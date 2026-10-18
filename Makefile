# WALKTHROUGH
# CI and the README use this file, not CMake.
# `make test` builds test_core and runs it, then the Python tests.
# Nothing in it opens a network connection. The process tests bind
# 127.0.0.1 on an ephemeral port to check the port guard.
# `make e2e` builds every binary, starts feedd and the adapter on the
# fixture file, waits one second, and runs headless. headless exits 0
# only when both sides of the book and the WAL are non-empty.
# `make start` is the live window. It needs a network and a terminal.
# The test targets (`test`, `e2e`, `rust-e2e`, `rust-interop`) use
# $(TEST_CONFIG), whose paths, port, pid files and logs are their own, so
# running one cannot stop the daemon of a window somebody has open. They used
# the live config and did exactly that: see docs/optimization/ledger.jsonl
# OPT-038.
# `make rust-feed` starts the feed and the adapter and no window, which is how a
# window that lost its feed gets one back: it re-attaches on its own. Do not
# reach for `make stop` — that stops the window too.
# start refuses when an instance is already running, and stop waits for
# each process to exit and reports any that did not. See
# docs/runbooks/port-already-in-use.md when a bind failure is reported.
# `make rust-build` and `make rust-test` build the Rust workspace under
# rust/ and run cargo fmt, clippy and its tests. `make rust-e2e` is `e2e`
# with --impl rust: the same fixture and the same headless check, driven by
# rust/target/release. `make rust-soak` runs the Rust cache soak over 1000
# symbols. `make rust-interop` is the cross-implementation proof: the C++
# feedd writes the ring and WAL and the Rust headless reads them, then the
# Rust feedd writes and the C++ headless reads. `make rust-vec-audit`
# `make rust-start` is `make start` with --impl rust: the same window, driven by
# rust/target/release. The C++ terminal still clears the screen on every frame,
# which erases the terminal's echo of what you type, so use rust-start if you want
# to type commands. `make stop` stops either implementation.
# compiles the hot kernels and fails when one that should vectorize did not.
CXX ?= c++
CXXFLAGS ?= -std=c++17 -O2 -pthread -Iinclude -Wall -Wextra
LDFLAGS ?= -pthread
BUILD := build
RUST := rust/target/release

# The live config: what a window runs on. A second window gets its own ring,
# WAL, port, pid files and logs by overriding this one variable, and the two
# then cannot reach each other.
CONFIG ?= config/dev.json

# The test config. Never the live one, for the reason in the header above.
TEST_CONFIG ?= config/test.json

.PHONY: all test e2e clean setup start stop rust-build rust-test rust-e2e rust-soak rust-interop rust-vec-audit rust-start rust-feed

all: $(BUILD)/feedd $(BUILD)/tui $(BUILD)/headless $(BUILD)/test_core

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/feedd: src/feedd.cpp include/ob/*.hpp | $(BUILD)
	$(CXX) $(CXXFLAGS) src/feedd.cpp -o $@ $(LDFLAGS)

$(BUILD)/tui: src/tui.cpp include/ob/*.hpp | $(BUILD)
	$(CXX) $(CXXFLAGS) src/tui.cpp -o $@ $(LDFLAGS)

$(BUILD)/headless: src/headless.cpp include/ob/*.hpp | $(BUILD)
	$(CXX) $(CXXFLAGS) src/headless.cpp -o $@ $(LDFLAGS)

$(BUILD)/test_core: tests/test_core.cpp include/ob/*.hpp | $(BUILD)
	$(CXX) $(CXXFLAGS) tests/test_core.cpp -o $@ $(LDFLAGS)

test: $(BUILD)/test_core
	$(BUILD)/test_core
	python3 -m unittest discover -s python -p 'test_*.py'

e2e: all
	python3 python/obctl.py setup --config $(TEST_CONFIG)
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	python3 python/obctl.py start --config $(TEST_CONFIG) --fixture e2e/fixture.jsonl
	sleep 1
	$(BUILD)/headless $(TEST_CONFIG) 1500
	python3 python/obctl.py stop --config $(TEST_CONFIG)

setup:
	python3 python/obctl.py setup --config $(CONFIG)

start:
	python3 python/obctl.py start --config $(CONFIG) --tui

stop:
	python3 python/obctl.py stop --config $(CONFIG)

rust-start:
	python3 python/obctl.py start --config $(CONFIG) --impl rust --tui

# The feed and the adapter only, in this terminal, for a window that has one
# open already. `rust-start` would open a second window; `stop` would close the
# first one. The window re-attaches by itself once feedd is publishing again.
rust-feed:
	python3 python/obctl.py start --config $(CONFIG) --impl rust

rust-build:
	cargo build --release --manifest-path rust/Cargo.toml

rust-test: rust-build
	cargo test --manifest-path rust/Cargo.toml
	cargo fmt --all --manifest-path rust/Cargo.toml --check
	cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings
	python3 -m unittest python.test_tui_input

rust-e2e: rust-build
	python3 python/obctl.py setup --config $(TEST_CONFIG) --impl rust
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	python3 python/obctl.py start --config $(TEST_CONFIG) --fixture e2e/fixture.jsonl --impl rust
	sleep 1
	$(RUST)/headless $(TEST_CONFIG) 1500
	python3 python/obctl.py stop --config $(TEST_CONFIG)

rust-soak: rust-build
	$(RUST)/obsoak --symbols 1000 --ticks 200

# Each half fails the make run when its headless exits non-zero, which is
# the whole check: a reader that cannot parse the other writer's ring or
# WAL stops the target before the PASS line prints.
rust-interop: all rust-build
	python3 python/obctl.py setup --config $(TEST_CONFIG) --impl cpp
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	python3 python/obctl.py start --config $(TEST_CONFIG) --fixture e2e/fixture.jsonl --impl cpp
	sleep 1
	$(RUST)/headless $(TEST_CONFIG) 1500
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	@echo "PASS: the Rust headless read the ring and WAL the C++ feedd wrote"
	python3 python/obctl.py setup --config $(TEST_CONFIG) --impl rust
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	python3 python/obctl.py start --config $(TEST_CONFIG) --fixture e2e/fixture.jsonl --impl rust
	sleep 1
	$(BUILD)/headless $(TEST_CONFIG) 1500
	python3 python/obctl.py stop --config $(TEST_CONFIG)
	@echo "PASS: the C++ headless read the ring and WAL the Rust feedd wrote"

rust-vec-audit:
	sh rust/scripts/vec-audit.sh

clean:
	rm -rf $(BUILD) var/dev var/prod var/test var/run
