.PHONY: build release install install-bin uninstall skill-install clean test fmt check fixtures help

# Default target
.DEFAULT_GOAL := help

# Variables
BINARY_NAME := resarch
INSTALL_PATH := /usr/local/bin
# AI エージェント側のスキルディレクトリ名 (skills/SKILL.md の frontmatter の name と揃える)
SKILL_NAME := resarch

## Build Commands

build: ## Build debug version
	cargo build

release: ## Build release version
	cargo build --release

## Installation

install: release ## Build release, install binary, and install skills (claude + codex)
	cp target/release/$(BINARY_NAME) $(INSTALL_PATH)/
	$(INSTALL_PATH)/$(BINARY_NAME) skill-install claude
	$(INSTALL_PATH)/$(BINARY_NAME) skill-install codex

install-bin: release ## Build release and install the binary only (no skills)
	cp target/release/$(BINARY_NAME) $(INSTALL_PATH)/

skill-install: ## Install the AI agent skill from the installed binary (claude + codex)
	$(INSTALL_PATH)/$(BINARY_NAME) skill-install claude
	$(INSTALL_PATH)/$(BINARY_NAME) skill-install codex

uninstall: ## Remove the installed binary and the installed skills
	rm -f $(INSTALL_PATH)/$(BINARY_NAME)
	rm -rf $(HOME)/.claude/skills/$(SKILL_NAME) $(HOME)/.codex/skills/$(SKILL_NAME)

## Development

test: ## Run tests
	cargo test

fixtures: ## Fetch upstream sysstat test data used by golden tests (not bundled: GPL)
	cargo run --quiet --bin xtask -- fetch-fixtures

fmt: ## Format code
	cargo fmt

check: ## Run clippy, check, and fmt check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo check
	cargo fmt -- --check

clean: ## Clean build artifacts
	cargo clean

## Help

help: ## Show this help message
	@echo "$(BINARY_NAME) Build Commands"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Release:"
	@echo "  Use GitHub Actions > Release > Run workflow"
