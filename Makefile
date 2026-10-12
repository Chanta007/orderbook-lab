# WALKTHROUGH
# CI and the README use this file, not CMake.
# `make test` builds test_core and runs it, then the Python tests.
# Nothing in it opens a network connection. The process tests bind
# 127.0.0.1 on an ephemeral port to check the port guard.
# `make e2e` builds every binary, starts feedd and the adapter on the
# fixture file, waits one second, and runs headless. headless exits 0
# only when both sides of the book and the WAL are non-empty.
# `make start` is the live window. It needs a network and a terminal.
# start refuses when an instance is already running, and stop waits for
# each process to exit and reports any that did not. See
# docs/runbooks/port-already-in-use.md when a bind failure is reported.
CXX ?= c++
CXXFLAGS ?= -std=c++17 -O2 -pthread -Iinclude -Wall -Wextra
LDFLAGS ?= -pthread
BUILD := build

.PHONY: all test e2e clean setup start stop

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
	python3 python/obctl.py setup --config config/dev.json
	python3 python/obctl.py stop --config config/dev.json
	python3 python/obctl.py start --config config/dev.json --fixture e2e/fixture.jsonl
	sleep 1
	$(BUILD)/headless config/dev.json 1500
	python3 python/obctl.py stop --config config/dev.json

setup:
	python3 python/obctl.py setup --config config/dev.json

start:
	python3 python/obctl.py start --config config/dev.json --tui

stop:
	python3 python/obctl.py stop --config config/dev.json

clean:
	rm -rf $(BUILD) var/dev var/prod var/run
