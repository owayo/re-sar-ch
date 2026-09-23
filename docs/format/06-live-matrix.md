# コンテナを使った live sa 採取マトリクス

本家 `sadc` が現在の Linux カーネルから生成する sa ファイルを、複数のディストリビューションと
sysstat 世代で採取するための開発用ランブックである。Apple Container と Docker の両方に対応する。

## 目的と制約

- ディストリビューションのパッケージ版を比較すると、各社が実際に配布している sysstat を確認できる。
- sysstat公式リリースをソースからビルドすると、ディストリビューションの更新時期に左右されず、形式世代の境界を確認できる。
- コンテナは Linux の `/proc` を読むため、macOS ホスト自体ではなくコンテナ VM の統計を採取する。
- 採取物には nodename が入る。このスクリプトはコンテナ名と hostname を `resarch-fixture` に固定し、通常の成果物を
  Git 管理外の `target/sar-matrix/` に保存する。
- 再現用に選別した `sa` と同じ版の `sar -A -C -t` のペアだけは、採取日ごとに
  `testdata/sysstat-live/` へ収録できる。必ず固定 hostname、使い捨てコンテナ VM、短い採取、
  SHA-256 付きの実測マニフェストを使い、運用ホストのデータと混ぜない。
- sysstat 自体は GPL-2.0-or-later である。収録スナップショットは独立に書き起こした MIT fixture
  とは別領域に置き、プロジェクト本体の fixture 生成元には使わない。

## 使い方

```bash
# 公式最新版だけ。2026-08-28 リリースの 12.8.0 固定commitのアーカイブをSHA-256照合してビルドする
make sar-latest

# Alpine / Debian / Ubuntu / Fedora / Rocky Linux の各パッケージ版
make sar-matrix

# format_magic 0x2171 / 0x2173 / 0x2175 初期 / 0x2175 現行
make sar-generations

# CentOS Vault の11ケース (3.9〜8.5、6.5 / 7.5を含む)
make sar-centos

# 記録済みの全20ケース (5 distro + 4 upstream + 11 CentOS)
make sar-all

# ケース名の確認と単体実行
scripts/collect-sar-matrix.sh --list
scripts/collect-sar-matrix.sh debian-13
scripts/collect-sar-matrix.sh centos-6.5
```

`container` があれば Apple Container を優先し、無ければ `docker` を使う。明示する場合は
`CONTAINER_RUNTIME=docker` のように指定する。採取間隔と点数も変更できる。

```bash
SAR_INTERVAL=5 SAR_COUNT=3 make sar-latest
```

各実行は UTC timestamp の新しいディレクトリを作り、既存成果物を上書きしない。
`SAR_MATRIX_OUTPUT` で保存先を指定できる (通常は既存ディレクトリを拒否する。再開は後述)。
ホストには Rust/Cargo、Bash、curl、jq、shasum とランタイムが必要。
コンテナは `resarch-fixture` という同じ名前で順番に起動するため、マトリクスを同時実行しない。
初回はイメージとビルド依存をダウンロードするため時間がかかる。

各ケースの進行状況は標準出力に、詳細は `collection.log` に保存する。
採取に失敗しても残りのケースを実行し、`SUMMARY.tsv` に `failed:<終了コード>` を残して
スクリプト全体も非ゼロ終了する。成功済みデータと失敗ログはどちらも保存される。
最古3ケースも reSARch で走査・描画する。読み取り失敗や末尾不一致は失敗として扱う。

## 成果物

実行ルートと各ケースのディレクトリに次を保存する。

| ファイル | 内容 |
|---|---|
| `SUMMARY.tsv` | 実行ルートに置く、版、magic、末尾一致、出力比較の一覧 |
| `READER-SHA256SUMS` | 全ケースの検証に使った `.tools/resarch` / `scan` / `verify_snapshot` のSHA-256 |
| `collection.log` | 各ケースのインストール・採取・検証ログ |
| `sa` | 各版の `sadc` で採取したバイナリ (`XALL` / `ALL` / 旧版の対応オプション。2.2は `sar` 自身で採取) |
| `PROVENANCE.txt` | イメージ、sysstat 版、ABI、SHA-256、採取条件 |
| `sar-A.txt` | 本家 `sar -A -C -t` の出力 (旧版では対応しているオプションだけを使用) |
| `SHA256SUMS` | `sa` と `sar-A.txt` の SHA-256。ケースのディレクトリで `shasum -a 256 -c SHA256SUMS` |
| `identify.json` | reSARchによる世代識別。読めない世代も記録する |
| `sadf-*` | 本家 `sadf` の各互換形式。未対応の版は stderr と失敗状態を残す |
| `resarch-scan.txt` | `examples/scan` の末尾一致検証。`exact=true` が必要 |
| `resarch-decode.txt` | 全activityの計画と全サンプルのデコード検証。`skipped=0`、有効な前後サンプル対、末尾一致が必要 |
| `resarch-sar-A.txt` | 同じ sa を reSARch で描画した結果 |
| `sar-A.diff` | 本家と reSARch が一致しない場合だけ作る差分 |

live 環境ではディスク名などの解決結果が実行環境に依存するため、差分があれば即不具合とは限らない。
まず `resarch-scan.txt` の `exact=true` を確認し、差分を activity 単位で分類する。

2026-09-24 に選別した 9 ケースの `sa` / `sar-A.txt` ペアは
[`testdata/sysstat-live/2026-09-24/`](../../testdata/sysstat-live/2026-09-24/) にあり、版、commit、
ABI、両ファイルの SHA-256 は
[`docs/measurements/sar-matrix-2026-09-24.tsv`](../measurements/sar-matrix-2026-09-24.tsv) に記録している。

## マトリクス

パッケージ版はリポジトリ更新に伴って版が変わる。その時点で実際に入った版を
`PROVENANCE.txt` に記録する。sysstat公式リリースは公式 GitHub タグと、そのタグが指す commit を固定する。
2026-09-23 時点で 12.8.0 の公式ダウンロードページ記載 SHA-1 と実配信アーカイブの SHA-1 が
一致しなかったため、チェックサム検証を省略せず、Git タグの commit 照合へ切り替えている。

| グループ | ケース |
|---|---|
| distro | Alpine 3.23、Debian 13、Ubuntu 24.04 LTS、Fedora 44、Rocky Linux 9 |
| generations | 10.2.1、11.6.6、12.0.6、12.8.0 |
| centos | 3.9、4.9、5.11、6.0、6.5、6.10、7.0、7.5、7.9、8.0、8.5 |

ここで「全ケース」は上表の記録済みケースを指し、各ディストリビューションの全歴代版を
意味しない。CentOS以外のパッケージ版は指定OSのリポジトリからその時点の版を取得する。

## CentOSの来歴と採取条件

[`tools/sar-matrix/centos.tsv`](../../tools/sar-matrix/centos.tsv) に、公式OCIイメージの
digest、Vaultのsysstat RPMと依存RPMのURL・SHA-256を固定する。ホスト側で取得・検証し、
コンテナ内でRPMをインストールして採取する。RPMキャッシュのハッシュ不一致は失敗とし、
古いコンテナ内のTLSやEOLのyumリポジトリ設定に依存しない。
CentOS 5.11はレジストリが返すamd64 manifestのdigestを使う。Apple Containerが
ローカルで生成するindexのdigestはレジストリに存在しないため、採取済み環境の
`image inspect` のindex値だけをそのまま固定してはいけない。

すべてx86_64のRPMを使う。Apple Siliconでは `--arch amd64 --rosetta`、Dockerでは
`--platform linux/amd64` を使う (ARMホストのDockerにはamd64実行環境が必要)。
公式イメージのrootfsとRPMの対象版が異なるケースがあるため、`PROVENANCE.txt` には
両方を記録する。RPMは `rpm -Uvh --nodeps` で使い捨て環境へ入れる。
これはsysstatバイナリとsa形式の採取であり、当時のOS全体や当時のカーネルの再現ではない。

| RPMの対象 | 使用rootfs |
|---|---|
| 3.9 / 4.9 / 5.11 | CentOS 5.11 |
| 6.0 / 6.5 | CentOS 6.6 |
| 6.10 | CentOS 6.10 |
| 7.0 / 7.5 / 7.9 | 対応するポイントリリース |
| 8.0 / 8.5 | digest固定の公式 `centos:8` (実際の版は採取メタデータに記録) |

最古3ケースでは `sadc INTERVAL COUNT sa` と `sar -A -t -f sa` を使う。
その他は `sadc -S XALL INTERVAL COUNT sa` と `sar -A -C -t -f sa`。
全ケース `LC_ALL=C TZ=UTC`、ホスト名 `resarch-fixture`、既定1秒間隔・2サンプル。
CentOSでは同じ `sar` による再読込結果もバイト比較し、ペアの整合性を確認する。
`0x2163` (CentOS 3.9 / 4.9) と `0x2169` (5.11) も reSARch で EOF と描画を検証する。
本家の旧版 sar と現行互換書式は異なるため、テキスト差分は採取失敗とはしない。
読み取り範囲は [旧 CentOS 形式の仕様](07-legacy-centos.md) を参照。

採取結果は自動的にGitへ追加しない。収録するケースの `sa` / `sar-A.txt` と来歴を
`testdata/sysstat-live/<採取日>/` へコピーし、ハッシュと検証状態を
`docs/measurements/` に記録してからコミットする。RPMやビルドソースは収録しない。

2026-09-24のCentOS 11ケースは [`centos-matrix-2026-09-24.tsv`](../measurements/centos-matrix-2026-09-24.tsv)
に記録した。対応するペアと `PROVENANCE.json` は `testdata/sysstat-live/2026-09-24/centos-*/` にある。

## sysstat公式リリースの選択

世代マトリクスの意図は次のとおり。

| sysstat | `format_magic` | 検証点 |
|---:|---:|---|
| 10.2.1 | `0x2171` | activity matrix 導入後の旧形式 |
| 11.6.6 | `0x2173` | RESTART で volatile activity が変わる旧形式の最終版 |
| 12.0.6 | `0x2175` | 自己記述形式の初期 |
| 12.8.0 | `0x2175` | 現在のsysstat公式リリース |

10.2.1 は現在の glibc では `major()` / `minor()` の宣言が自動で入らないため、古い Makefile が
実際にコンパイルへ渡す `CFLAGS` 経由で `-include sys/sysmacros.h` を与える。ソース、sa 形式、
計算順序には変更を加えない。

## 公式 Git リリースを全件検証する

```bash
make sar-upstream-all
# 版を指定して再実行する
scripts/collect-sar-matrix.sh upstream-6.1.1
scripts/collect-sar-matrix.sh upstream-11.2.1.1
# 同じ採取物を、更新した reSARch で再検証する。未採取ケースは採取する。
SAR_MATRIX_OUTPUT=target/sar-matrix/<前回の実行> SAR_MATRIX_RESUME=1 make sar-upstream-all
# 公式 Git タグ一覧を更新する (ホストの Python 3 と Git が必要)
python3 tools/sar-matrix/update-upstream.py
```

[`upstream.tsv`](../../tools/sar-matrix/upstream.tsv) に対象を固定する。
2026-09-24時点では公式 Git リポジトリの134タグを133版にまとめたもの
(9.1.5〜12.8.0、4段番号の11.2.1.1を含む) と、Git移行前のアーカイブ48版
(2.2〜9.0.6.1) の合計181ケースを収録する。同じcommitを指す11.1.5のタグ別名は
重複実行しない。**公式 Git に残る全リリース**を対象とするが、Git移行前の
すべての歴代リリースのアーカイブが揃っているという意味ではない。
`all` / `make sar-all` は従来どおりディストリビューションを含む20ケースである。

各行はソースURL、SHA-256、Git版ではタグの参照先commitを固定する。
古いSRPMはSRPM全体と中に含まれる公式ソースアーカイブの両方を検証し、
ディストリビューションのパッチを適用せずに展開する。
ホストの `target/sar-matrix/sources/` を再利用するが、使用前のSHA-256照合は省略しない。
キャッシュが壊れている場合は失敗する。自動的に期待値を書き換えることはない。

共通のcompilerイメージはdigest固定のDebian 12とCコンパイラを含み、初回だけ構築する。
各版はこのイメージでコンパイルし、実行に必要なsar/sadc/sadfだけをDebian runtimeへ移した
`resarch-sysstat-<版>:<ビルド入力のハッシュ>` を作り、
固定hostnameの使い捨てコンテナで採取する。版ごとにaptを実行しない。
ソースのcommit/SHA-256、ビルドスクリプト、compilerイメージが変わるとイメージ名も変わる。
これらは当時のOS・カーネルを再現するイメージではなく、公式sysstatの各版を現在のLinuxで実行する検証環境である。

GCC/glibcとの互換性のため、旧版にはページサイズ定義、パス定義、IRQ配列の関数宣言、
キャストしたポインタへの代入構文の修正を適用する。構造体のフィールドや統計計算式は変更しない。
適用内容は [`build-release.sh`](../../tools/sar-matrix/build-release.sh) と採取来歴に残す。
古いMakefileは同じ静的ライブラリを並行更新するため、旧版は直列ビルドする。
2.2は独立したsadcがないため、当該版のsar自身で記録・再読込する。
各版で利用可能な採取オプションを使い、同じ本家sarによる2回の描画が一致することも確認する。

`SAR_MATRIX_RESUME=1` では保存済み `SHA256SUMS` を照合してから、reSARchの識別・
EOF走査・全activityのデコード・描画をもう一度実行する。成功済みという理由で検証を省略しない。
採取条件を変えて採り直す場合は新しい出力ディレクトリを指定する。
失敗しても残りを続け、1件でも失敗があれば全体を非ゼロ終了する。
`SUMMARY.tsv` の `scan_exact=true` は末尾までのレコード走査が完了したことを示す。
旧版sarとのテキスト一致や全フィールドの解釈を意味しない。
`activity_decode=complete` は、宣言された全activityについてデコード計画があり、
読み飛ばしが0件で、全サンプルのデコードが末尾まで成功し、有効な前後サンプル対も得られたことを示す。
専用の `examples/verify_snapshot.rs` で検証し、未知のactivity IDやmagicによる読み飛ばしも失敗とする。
EOF走査成功、全activityのデコード成功、reSARchによる描画成功は必須で、
本家とのテキスト差分は別列に記録する。

事前に構築したcompilerイメージを使う場合は `SAR_COMPILER_IMAGE=<イメージ名>` を指定する。
指定イメージはローカルに存在し、Containerfile.compiler相当のツールを備える必要がある。
イメージ名だけでなく実際のdigestも版別イメージのキャッシュキーに含める。

版別イメージは、既定ではそのケースの終了時に削除する（採取・検証失敗時も同じ）。
削除するのは今回のケースが新規作成したイメージだけで、事前に存在するイメージ、
共通compilerイメージ、ソースキャッシュ、採取成果物は残す。
全版の展開済みイメージを同時保持してディスクを圧迫することを避けるためである。
繰り返し採取するためにイメージも残す場合は `SAR_MATRIX_KEEP_IMAGES=1` を指定する。

実行開始時にreSARch、scan、verify_snapshotを一度だけビルドし、実行ディレクトリの `.tools/` にコピーする。
全ケースを同じ実行ファイルで検証し、`READER-SHA256SUMS` にそのSHA-256を残す。
採取中に別ターミナルでビルドしても途中からreaderが切り替わらず、ケースごとにCargoのロックを待つこともない。
resume時には新しくビルドしたreaderを使い、保存済み全ケースも再検証する。

9.0.0の本家sarには、CPUとCPU周波数が共有するbitmapを二重解放して終了時にabortする不具合がある。
この版だけは公式9.0.1の修正と同じ、解放後にポインタをNULLへ戻す2行を適用する。
採取形式・統計式は変更せず、`PROVENANCE.txt` の `native_backport` に適用を明記する。

ランチャーの失敗継続、SHA-256不一致拒否、生成イメージの所有範囲と削除失敗の報告は、
`python3 tools/sar-matrix/test-launcher.py` でコンテナやネットワークなしに検証できる。

2026-09-24の全181ケースは、採取、本家sarの再読込一致、reSARchの末尾一致と描画に成功した。
[測定表](../measurements/upstream-matrix-2026-09-24.tsv) と
[sa / sarテキスト / 来歴 / SHA-256](../../testdata/sysstat-live/2026-09-24-official-all/) をGit管理している。
`cargo test --test official_snapshots` で収録済み全件の走査・デコードも再検証できる。
