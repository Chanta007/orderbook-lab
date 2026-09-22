# CI and the README use this file, not CMake. test is offline. e2e replays
# e2e/fixture.jsonl through feedd and checks headless counts.
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
	rm -rf $(BUILD) var/dev var/run
