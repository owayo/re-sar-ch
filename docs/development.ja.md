# 開発

標準の `make` ターゲットは [README](../README.ja.md#開発) にあります。このページには、それ以外のターゲット、本家のデータを使うテスト、採取スクリプト、リリースの出し方をまとめます。

## ツールチェーン

[mise](https://mise.jdx.dev/) が必要です。ツールの版は `mise.toml` で固定しています。`make` の各ターゲットは `mise exec` 経由でツールを呼ぶので、シェルで mise を有効にしていなくても固定した版で動きます。mise を使わない場合は `SYSTEM_TOOLS=1` を付けると PATH 上のツールを使います。その場合、版が CI と一致するとは限りません。

Windows では、チェックアウトから `cargo install --path . --locked --bin resarch` でビルドして入れます。リポジトリには開発用の `xtask` バイナリも入っているため、`--bin resarch` で CLI だけを選びます。

## ほかのターゲット

引数なしの `make` で全ターゲットの一覧が出ます。標準のターゲットのほかに、次のものがあります。

| コマンド | 説明 |
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

説明は `make help` の出力そのままです。`make install` はバイナリを入れたあと Claude Code と Codex 向けのスキルも書き出します。`make install SKILL_TARGETS=` でスキルを入れずに済みます。`make uninstall` が消すのはバイナリだけで、スキルは残ります。

## 適合テスト

`make conformance` は `make fixtures` で本家のデータを取得してから、`make test` では飛ばす適合テストを回します。`xmllint` が必要です。macOS には標準で入っており、Debian / Ubuntu では `libxml2-utils` パッケージに含まれます。sysstat 本体の `sar` の版を記録するテストは、`sar` が無い環境ではスキップします。CI は Ubuntu 24.04 に sysstat を入れ、比較の基準となる版を固定したうえでジョブのサマリーに記録しています。

## 実際の sa ファイルを採取する

`sar-*` のターゲットは、コンテナの中で実際の `sa` ファイルを採取して検証します ([出力の正確さをどう検証しているか](verification.ja.md)を参照)。Apple container か Docker に加えて、ホスト側に `jq`・`curl`・`shasum` が必要です。

## テストデータ

本家 sysstat は GPL なので、**そのテストデータも期待出力もこのリポジトリには同梱していません**。`make fixtures` が固定タグから SHA-256 検証つきで `target/fixtures/` へ取得し、適合テストのときだけ使います。`tests/fixtures/` は独自に書き起こしたテストデータで、本体と同じ MIT ライセンスです。実採取した `testdata/sysstat-live/` のファイルとは区別しています。実採取データの来歴と扱いは[スナップショットの注意事項](../testdata/sysstat-live/README.md)を参照してください。

reSARch は `sysstat` のソースを移植したものではなく、ディスク上のフォーマットから独立に実装したものです。

## リリース

GitHub Actions の **Actions > Release > Run workflow** から出します。workflow は `Cargo.toml` と `Cargo.lock` の版を上げてタグを打ち、[GitHub Releases から](../README.ja.md#github-releases-から)に挙げた 6 種類のアーカイブをビルドして GitHub Release を公開します。続けて同じ workflow が Homebrew の tap を更新し、winget のマニフェストを提出します。

- **dry_run** をオンにすると、次の版を計算するだけで終わります。コミット、タグ、ビルド、公開はしません。
- 版は CalVer の `YY.M.PATCH` (例: `26.9.106`) で、`PATCH` は毎月 100 から始まります。番号からは互換性を壊す変更かどうかが読み取れないので、更新する前に [CHANGELOG.md](../CHANGELOG.md) の各版の先頭にある「⚠️ 破壊的変更」を確認してください。
