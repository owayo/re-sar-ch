# resarch の開発用タスク。引数なしの `make` でターゲット一覧を表示する。
#
# ツールの版は mise.toml が正。mise があればコマンドを `mise exec --` 経由で呼ぶので、
# シェルで mise を activate していなくても (IDE や GUI から make を呼んでも)
# mise.toml の版で動く。mise を使わず PATH 上のツールで動かすなら SYSTEM_TOOLS=1 を
# 付ける (その場合、版の再現性は保証しない)。
#
# CI の Test ジョブは make setup と make ci だけを呼ぶ。検査を変えるときは ci
# (と、それが呼ぶターゲット) を直し、CI の workflow に検査コマンドを重ねて書かない。
#
# macOS 標準の GNU Make 3.81 で動く書き方に限っている
# (.ONESHELL / .SHELLFLAGS / $(file ...) / != は使わない)。

.DEFAULT_GOAL := help

# Variables
BINARY_NAME := resarch
INSTALL_PATH ?= /usr/local/bin
# AI エージェント側のスキルディレクトリ名 (skills/SKILL.md の frontmatter の name と揃える)
SKILL_NAME := resarch
# install / skill-install でスキルを入れる AI エージェント。make install SKILL_TARGETS= で入れない
SKILL_TARGETS ?= claude codex
# Cargo.lock をコミットしているので、依存の解決結果を CI とそろえる
CARGO_FLAGS ?= --locked

# ---- ツールチェーン -----------------------------------------------------------
# mise は PATH、よくある導入先の順に探す。GUI から起動した make はシェルの PATH を
# 引き継がないことがあるため。make MISE=/path/to/mise で明示もできる。
# mise が無い環境の振る舞いを試すときは MISE_CANDIDATES= で探す先を空にする。
MISE_CANDIDATES ?= $(HOME)/.local/bin/mise /opt/homebrew/bin/mise /usr/local/bin/mise
ifeq ($(SYSTEM_TOOLS),1)
RUN :=
else
ifndef MISE
MISE := $(firstword $(shell command -v mise 2>/dev/null) $(wildcard $(MISE_CANDIDATES)))
endif
ifeq ($(MISE),)
ifneq ($(filter-out help,$(or $(MAKECMDGOALS),help)),)
$(error mise not found. Install it from https://mise.jdx.dev, or add SYSTEM_TOOLS=1 to use the tools on PATH)
endif
endif
RUN := $(if $(MISE),$(MISE) exec --,)
endif

.PHONY: help setup build release run install install-bin skill-install uninstall \
	test lint fmt fmt-check check ci fixtures conformance bench \
	sar-latest sar-matrix sar-generations sar-centos sar-all sar-upstream-all clean

## Setup

setup: ## Install the toolchain (mise.toml) and fetch dependencies
	@if [ -n "$(MISE)" ]; then "$(MISE)" install; fi
	$(RUN) cargo fetch $(CARGO_FLAGS)

## Build Commands

build: ## Build debug version
	$(RUN) cargo build $(CARGO_FLAGS)

release: ## Build release version
	$(RUN) cargo build --release $(CARGO_FLAGS)

run: ## Run the debug build (pass arguments with ARGS="...")
	$(RUN) cargo run $(CARGO_FLAGS) --bin $(BINARY_NAME) -- $(ARGS)

## Installation

# スキルは入れたばかりのバイナリで書き出すので、バイナリとスキルの版がそろう
install: install-bin ## Build release, install binary, and install skills (claude + codex)
	@for target in $(SKILL_TARGETS); do \
		"$(INSTALL_PATH)/$(BINARY_NAME)" skill-install "$$target" || exit 1; \
	done

# 上書きコピーではなく一時ファイル + rename で置き換える。macOS はコード署名の
# 検証結果を inode 単位でキャッシュするため、実行中や直前に実行したバイナリへ cp で
# 上書きすると、新しいバイナリが起動直後に SIGKILL される (exit 137)。
# 一時ファイルは rename が inode の差し替えになるよう、同じディレクトリに置く。
install-bin: release ## Build release and install the binary only (no skills)
	@mkdir -p "$(INSTALL_PATH)"
	cp "target/release/$(BINARY_NAME)" "$(INSTALL_PATH)/$(BINARY_NAME).new"
	mv -f "$(INSTALL_PATH)/$(BINARY_NAME).new" "$(INSTALL_PATH)/$(BINARY_NAME)"

skill-install: ## Install the AI agent skill from the installed binary (claude + codex)
	@for target in $(SKILL_TARGETS); do \
		"$(INSTALL_PATH)/$(BINARY_NAME)" skill-install "$$target" || exit 1; \
	done

# スキルの置き場所は、バイナリの skill-install が書き出す先 (~/.claude と ~/.codex) に合わせている
uninstall: ## Remove the installed binary and the installed skills
	rm -f "$(INSTALL_PATH)/$(BINARY_NAME)"
	rm -rf "$(HOME)/.claude/skills/$(SKILL_NAME)" "$(HOME)/.codex/skills/$(SKILL_NAME)"

## Development

# CI の Test ジョブと同じフラグ (--all-features) を付ける。いまは feature が無いので効果は無い
test: ## Run tests
	$(RUN) cargo test $(CARGO_FLAGS) --all-features

lint: ## Run clippy with warnings as errors
	$(RUN) cargo clippy $(CARGO_FLAGS) --all-targets --all-features -- -D warnings

fmt: ## Format code
	$(RUN) cargo fmt --all

fmt-check: ## Check formatting (no rewrite)
	$(RUN) cargo fmt --all -- --check

# lint は --all-features で回すので、既定の feature でのコンパイルもここで確かめる
check: fmt-check lint ## Run fmt check, clippy, and check (no rewrite)
	$(RUN) cargo check $(CARGO_FLAGS)

ci: check test ## Run the same checks as the CI Test job (no rewrite)

fixtures: ## Fetch upstream sysstat test data used by golden tests (not bundled: GPL)
	$(RUN) cargo run $(CARGO_FLAGS) --quiet --bin xtask -- fetch-fixtures

# 本家データを取得して ignored のテストを回す (CI の Conformance ジョブと同じ)。
# sar を呼ぶテストは、sysstat が無い環境ではスキップと表示して通る。detect_svg は xmllint を使う
conformance: fixtures ## Run conformance tests against upstream sysstat data (fetches fixtures)
	$(RUN) cargo test $(CARGO_FLAGS) --test conformance -- --include-ignored --nocapture
	$(RUN) cargo test $(CARGO_FLAGS) --test sa2sar -- --include-ignored --nocapture
	$(RUN) cargo test $(CARGO_FLAGS) --test detect_svg -- --include-ignored --nocapture

bench: ## Run benchmarks (set RESARCH_BENCH_FILE, or run make fixtures first)
	$(RUN) cargo bench $(CARGO_FLAGS)

## Data Collection

# 採取スクリプトはホストの cargo でビルドするので、mise の版で動かす
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

clean: ## Clean build artifacts
	$(RUN) cargo clean

## Help

help: ## Show this help message
	@echo "$(BINARY_NAME) Build Commands"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Tool versions are pinned in mise.toml. Run make setup first."
	@echo "Without mise, add SYSTEM_TOOLS=1 to use the tools on PATH."
	@echo ""
	@echo "Release:"
	@echo "  Use GitHub Actions > Release > Run workflow"
