# Development tasks for resarch. Run `make` with no arguments to list the targets.
#
# Tool versions are pinned in mise.toml. When mise is available, every tool runs through
# `mise exec --`, so the pinned versions are used even when mise is not activated in the shell
# (for example when make is started from an IDE or a GUI). SYSTEM_TOOLS=1 uses the tools on PATH
# instead (the versions are then not guaranteed).
#
# The Quality job of CI runs only make setup and make ci. To change the checks, edit ci (and the
# targets it calls) instead of adding commands to the CI workflow.
#
# Only GNU Make 3.81 features are used (the make that ships with macOS):
# no .ONESHELL, .SHELLFLAGS, $(file ...) or !=.

.DEFAULT_GOAL := help

BINARY_NAME := resarch
INSTALL_PATH ?= /usr/local/bin
# AI agents that get the skill from install / skill-install. make install SKILL_TARGETS= skips the skills
SKILL_TARGETS ?= claude codex
# Cargo.lock is committed, so resolve dependencies exactly as CI does
CARGO_FLAGS ?= --locked

# ---- Toolchain ------------------------------------------------------------------
# Look for mise on PATH, then in the usual install locations (make started from a GUI may not
# inherit the shell's PATH). Override with make MISE=/path/to/mise.
# To try the behavior without mise, empty the candidates with MISE_CANDIDATES=.
MISE_CANDIDATES ?= $(HOME)/.local/bin/mise /opt/homebrew/bin/mise /usr/local/bin/mise
ifeq ($(SYSTEM_TOOLS),1)
RUN :=
else
ifndef MISE
MISE := $(firstword $(shell command -v mise 2>/dev/null) $(wildcard $(MISE_CANDIDATES)))
endif
ifeq ($(MISE),)
ifneq ($(filter-out help,$(or $(MAKECMDGOALS),help)),)
$(error mise was not found. Install it from https://mise.jdx.dev, or add SYSTEM_TOOLS=1 to use the tools on PATH)
endif
endif
RUN := $(if $(MISE),$(MISE) exec --,)
endif

.PHONY: help setup build release run install install-bin skill-install uninstall \
	test lint fmt fmt-check check ci fixtures conformance bench \
	sar-latest sar-matrix sar-generations sar-centos sar-all sar-upstream-all clean

## Setup

setup: ## Install the toolchain (mise) and dependencies
	@if [ -n "$(MISE)" ]; then "$(MISE)" install; fi
	$(RUN) cargo fetch $(CARGO_FLAGS)

## Build

build: ## Build a debug binary
	$(RUN) cargo build $(CARGO_FLAGS)

release: ## Build a release binary
	$(RUN) cargo build --release $(CARGO_FLAGS)

run: ## Run the debug binary (arguments via ARGS="...")
	$(RUN) cargo run $(CARGO_FLAGS) --bin $(BINARY_NAME) -- $(ARGS)

## Install

# The skills are written by the binary that was just installed, so the binary and the skills
# always come from the same version
install: install-bin ## Install the release binary to INSTALL_PATH (default /usr/local/bin) and the agent skills (claude, codex)
	@for target in $(SKILL_TARGETS); do \
		"$(INSTALL_PATH)/$(BINARY_NAME)" skill-install "$$target" || exit 1; \
	done

# Replace the binary through a temporary file and a rename instead of copying over it. macOS
# caches the code signature check per inode, so a binary copied over one that is running (or ran
# a moment ago) is killed with SIGKILL right after it starts (exit 137). The temporary file sits
# in the same directory so that the rename swaps the inode.
install-bin: release ## Install the release binary to INSTALL_PATH without the agent skills
	@mkdir -p "$(INSTALL_PATH)"
	cp "target/release/$(BINARY_NAME)" "$(INSTALL_PATH)/$(BINARY_NAME).new"
	mv -f "$(INSTALL_PATH)/$(BINARY_NAME).new" "$(INSTALL_PATH)/$(BINARY_NAME)"

skill-install: ## Write the AI agent skill with the installed binary (SKILL_TARGETS, default claude codex)
	@for target in $(SKILL_TARGETS); do \
		"$(INSTALL_PATH)/$(BINARY_NAME)" skill-install "$$target" || exit 1; \
	done

# Only the binary is removed. The skills stay in each agent's skills directory (for example
# ~/.claude/skills/resarch): the location differs per agent, and a skill there may belong to
# another installed copy of resarch. Delete that directory by hand when it is no longer needed.
uninstall: ## Remove the binary from INSTALL_PATH
	rm -f "$(INSTALL_PATH)/$(BINARY_NAME)"

## Checks

test: ## Run the tests
	$(RUN) cargo test $(CARGO_FLAGS)

lint: ## Run clippy with warnings as errors
	$(RUN) cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings

fmt: ## Format the code (rewrites files)
	$(RUN) cargo fmt --all

fmt-check: ## Check the formatting (no changes)
	$(RUN) cargo fmt --all -- --check

check: fmt-check lint ## Run fmt-check and lint (no changes)

ci: check test ## Run the same checks as CI (no changes)

## Conformance

fixtures: ## Fetch upstream sysstat test data used by golden tests (not bundled: GPL)
	$(RUN) cargo run $(CARGO_FLAGS) --quiet --bin xtask -- fetch-fixtures

# Fetch the upstream data and run the ignored tests (the same as the Conformance job of CI).
# The tests that call sar pass with a skip notice where sysstat is not installed; detect_svg uses xmllint
conformance: fixtures ## Run conformance tests against upstream sysstat data (fetches fixtures)
	$(RUN) cargo test $(CARGO_FLAGS) --test conformance -- --include-ignored --nocapture
	$(RUN) cargo test $(CARGO_FLAGS) --test sa2sar -- --include-ignored --nocapture
	$(RUN) cargo test $(CARGO_FLAGS) --test detect_svg -- --include-ignored --nocapture

bench: ## Run benchmarks (set RESARCH_BENCH_FILE, or run make fixtures first)
	$(RUN) cargo bench $(CARGO_FLAGS)

## Data collection

# The collection script builds with the host's cargo, so run it with the pinned toolchain
sar-latest: ## Collect and compare an sa file with the latest upstream sysstat
	$(RUN) ./scripts/collect-sar-matrix.sh latest

sar-matrix: ## Collect sa files from multiple Linux distribution packages
	$(RUN) ./scripts/collect-sar-matrix.sh distro

sar-generations: ## Collect sa files across upstream sysstat format generations
	$(RUN) ./scripts/collect-sar-matrix.sh generations

sar-centos: ## Collect eleven CentOS Vault RPM releases, including 6.5 and 7.5
	$(RUN) ./scripts/collect-sar-matrix.sh centos

sar-all: ## Collect all twenty recorded distribution and upstream cases
	$(RUN) ./scripts/collect-sar-matrix.sh all

sar-upstream-all: ## Build and verify every pinned official sysstat source release
	$(RUN) ./scripts/collect-sar-matrix.sh upstream-all

## Cleanup

clean: ## Remove build artifacts
	$(RUN) cargo clean

## Help

help: ## Show this help
	@echo "Development tasks for $(BINARY_NAME)"
	@echo ""
	@echo "Usage: make <target>"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Tool versions are pinned in mise.toml. Run make setup first."
	@echo "Without mise, add SYSTEM_TOOLS=1 to use the tools on PATH."
	@echo "Release: GitHub Actions > Release > Run workflow"
