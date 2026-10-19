# Development

The [README](../README.md#development) lists the standard `make` targets. This page covers the rest: the targets beyond the standard ones, the tests that need upstream data, the collection scripts, and how releases are made.

## Toolchain

Requires [mise](https://mise.jdx.dev/). Tool versions are pinned in `mise.toml`. Every `make` target runs its tools through `mise exec`, so mise does not have to be activated in your shell. Without mise, add `SYSTEM_TOOLS=1` to use the tools on your PATH; their versions may then differ from CI.

On Windows, build and install from a checkout with `cargo install --path . --locked --bin resarch`. The repository also contains `xtask`, a development-only binary, so `--bin resarch` picks the CLI.

## Other targets

Run `make` with no arguments to list every target. Besides the standard ones:

| Command | Description |
|---|---|
| `make install-bin` | Install the release binary to INSTALL_PATH without the agent skills |
| `make skill-install` | Write the AI agent skill with the installed binary (SKILL_TARGETS, default claude codex) |
| `make fixtures` | Fetch upstream sysstat test data used by golden tests (not bundled: GPL) |
| `make conformance` | Run conformance tests against upstream sysstat data (fetches fixtures) |
| `make bench` | Run benchmarks (set RESARCH_BENCH_FILE, or run make fixtures first) |
| `make sar-latest` | Collect and compare an sa file with the latest upstream sysstat |
| `make sar-matrix` | Collect sa files from multiple Linux distribution packages |
| `make sar-generations` | Collect sa files across upstream sysstat format generations |
| `make sar-centos` | Collect eleven CentOS Vault RPM releases, including 6.5 and 7.5 |
| `make sar-all` | Collect all twenty recorded distribution and upstream cases |
| `make sar-upstream-all` | Build and verify every pinned official sysstat source release |

`make install` writes the skill for Claude Code and Codex after installing the binary; `make install SKILL_TARGETS=` skips it. `make uninstall` removes only the binary and leaves the skills in place.

## Conformance tests

`make conformance` downloads the upstream data with `make fixtures` and then runs the suites that `make test` skips. It needs `xmllint`, which macOS ships and Debian / Ubuntu provide in `libxml2-utils`. The test that records the version of sysstat's own `sar` skips itself when `sar` is not installed; CI installs sysstat on Ubuntu 24.04 so that the baseline it compares against stays fixed and is recorded in the job summary.

The byte-reader and writer regression tests cover out-of-bounds access, range and field-offset additions involving `usize::MAX`, empty buffers, and zero-length access exactly at the end of a buffer. Rejected writes must preserve the existing buffer. The tests run as part of `make ci` and use the target's `usize` width for the boundary values.

The library's optional gauge filling (`GaugeFill::HoldLast` / `Linear`) uses finite observations without exclusion reasons. If no eligible observation is available, the bucket remains `NoObservation` with no value. Regression tests cover NaN and both infinities on either interpolation endpoint and on the value carried forward.
The interpolation midpoint is also checked for one-second buckets and epochs near `u64::MAX`.

A fixture with a CPU-count change at RESTART checks segment metadata and the run-queue verdict. SVG tests cover positional interval/count, rates between selected samples, count resets at RESTART, and line breaks between boot segments.
The `-O oneday` test also keeps observations beyond the first 24 hours.

When comparing operational logs with saved `sar` text, select the rendering profile for the producer's version explicitly. The [local comparison procedure](format/05-sysstat-10.1.5-el7.md#61-実データ) (Japanese) provides an el7 example and distinguishes a full-text match from a scan reaching the end of the file.

## Collecting real sa files

The `sar-*` targets collect and verify real `sa` files inside containers (see [How the output is verified](verification.md)). They need Apple container or Docker, plus `jq`, `curl`, and `shasum` on the host.

## Test data

Upstream `sysstat` is GPL-licensed, so **none of its test data or expected output is vendored here**. `make fixtures` fetches it into `target/fixtures/` at a pinned tag with SHA-256 verification, for the conformance suite only. The data in `tests/fixtures/` is written from scratch and MIT-licensed like the rest of the project. Live snapshots in `testdata/sysstat-live/` are separate; see their [provenance and usage notice](../testdata/sysstat-live/README.md).

reSARch is an independent implementation written from the on-disk format, not a port of `sysstat` source.

## Releases

Releases are made from GitHub Actions: **Actions > Release > Run workflow**. The workflow bumps the version in `Cargo.toml` and `Cargo.lock`, tags the commit, builds the six archives listed under [From GitHub Releases](../README.md#from-github-releases), and publishes the GitHub Release. The same workflow then updates the Homebrew tap and submits the winget manifest.

- Turn on **dry_run** to compute the next version only. Nothing is committed, tagged, built, or published.
- Versions use CalVer, `YY.M.PATCH` (for example `26.9.106`), where `PATCH` starts at 100 each month. The number does not show whether a release breaks compatibility, so read the "⚠️ 破壊的変更" (breaking changes) entries that open each version in [CHANGELOG.md](../CHANGELOG.md) before upgrading.
