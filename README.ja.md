<p align="center">
  <img src="docs/images/app.png" width="128" alt="reSARch">
</p>

<h1 align="center">re<strong>SAR</strong>ch</h1>

<p align="center">
  sysstat の sa バイナリを単体で解析する CLI。異変の検知、SVG グラフ、sar / sadf 互換出力を、Linux・macOS・Windows のどれでも実行できます
</p>

<!-- standard:badges:start -->
<h3 align="center">対応プラットフォーム</h3>

<p align="center">
  <img src="https://img.shields.io/badge/Linux-FCC624?logo=linux&amp;logoColor=black" alt="Linux">
  <img src="https://img.shields.io/badge/macOS-000000?logo=apple&amp;logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Windows-0078D6" alt="Windows">
</p>

<p align="center">
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/owayo/re-sar-ch/releases/latest"><img src="https://img.shields.io/github/v/release/owayo/re-sar-ch" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/owayo/re-sar-ch" alt="License"></a>
</p>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.ja.md">日本語</a>
</p>
<!-- standard:badges:end -->

---

Linux で採取した `sa` ファイルを、手元の PC で調べるためのツールです。解析する PC に `sar` / `sadf` をインストールする必要はありません。

`sar` のログ (`/var/log/sa/saXX`) は C 構造体をそのまま書き出した形式で、読むには通常、互換性のある世代・アーキテクチャの sysstat が必要です。reSARch は対応形式のバイト列を直接読み、構造体の配置をファイルを書いたホストの ABI に合わせて解釈します。設計の要点は [docs/architecture.ja.md](docs/architecture.ja.md) にあります。

## 機能

- **sar / sadf 互換の出力**: 本家 sysstat 12.8.0 の書式によるテキスト・JSON・CSV・XML。`--sar-profile` で RHEL / CentOS 7 の `sar` の書式にも切り替え可能
- **TUI**: 記録されたすべての activity を端末上の表とグラフで閲覧
- **分析**: 異変の検知、期間集計、複数ホストの比較、検知した箇所の SVG グラフ
- **登録済み 28 形式**: sysstat 2.2 から 12.8 系までの読み取り。big-endian・32 bit のホストが書いたファイルにも対応 ([旧形式の制限](docs/formats.ja.md)あり)
- **採取と検証**: 各ディストリビューションのパッケージと sysstat 公式リリースのイメージによる自動採取・検証 ([出力の検証方法](docs/verification.ja.md))

## 動作環境

Linux 上の sysstat で採取した `sa` ファイルを用意してください。reSARch は保存済みのファイルを解析します。新しく採取する場合は、Linux 上の sysstat か同梱の[採取スクリプト](docs/development.ja.md#実際の-sa-ファイルを採取する)を使います。TUI には対話端末が必要です。

## インストール

<!-- standard:install:start -->
### Homebrew (macOS/Linux)

```bash
brew install owayo/re-sar-ch/re-sar-ch
```

### winget (Windows)

```powershell
winget install owayo.reSARch
```

### Cargo

Rust 1.98 以上が必要です。

```bash
cargo install --git https://github.com/owayo/re-sar-ch --bin resarch --locked
```

### GitHub Releases から

[Releases](https://github.com/owayo/re-sar-ch/releases/latest) から自分の環境のアーカイブを取得して展開し、`resarch` を `PATH` の通った場所に置きます。各リリースには、取得したファイルを確かめるための `SHA256SUMS` も添付しています。

| プラットフォーム | ファイル |
|---|---|
| Linux (x86_64) | `resarch-x86_64-unknown-linux-gnu.tar.gz` |
| Linux (x86_64, musl) | `resarch-x86_64-unknown-linux-musl.tar.gz` |
| Linux (ARM64) | `resarch-aarch64-unknown-linux-gnu.tar.gz` |
| macOS (Intel) | `resarch-x86_64-apple-darwin.tar.gz` |
| macOS (Apple Silicon) | `resarch-aarch64-apple-darwin.tar.gz` |
| Windows (x86_64) | `resarch-x86_64-pc-windows-msvc.zip` |

macOS でブラウザから取得した場合は、実行の前に隔離属性を外します: `xattr -d com.apple.quarantine resarch`。

### ソースから

[mise](https://mise.jdx.dev/) が必要です (Rust のツールチェーンは `mise.toml` で固定しています)。

```bash
git clone https://github.com/owayo/re-sar-ch.git
cd re-sar-ch
make install
```

`make install` は `/usr/local/bin` に入れます。場所を変えるときは `INSTALL_PATH` を指定します (例: `make install INSTALL_PATH="$HOME/.local/bin"`)。
<!-- standard:install:end -->

`make install` は、Claude Code と Codex 向けの resarch スキルも `~/.claude/skills/resarch` と `~/.codex/skills/resarch` に書き出します。`make install SKILL_TARGETS=` とするとスキルを入れず、`make install-bin` はバイナリだけを入れます。ほかの方法で入れた場合は、`resarch skill-install` でスキルを書き出します ([AI エージェントから使う](#ai-エージェントから使う))。

winget でインストールすると、`PATH` への登録も自動で行われます。インストール後は、変更を反映するために新しいターミナルを開いてください。

## クイックスタート

インストール後、`sa01` を手元のファイルのパスに置き換えて実行します。Linux 以外で解析する場合は、採取元からファイルをコピーしてください。

```bash
resarch info sa01               # ファイルの形式と記録項目を確認
resarch -u -f sa01              # CPU 使用率を表示
resarch detect sa01             # 調査する時刻と指標の候補を表示
```

記録された統計の種類 (CPU、メモリなど) を **activity**、CPU やデバイスなど個々の対象を **item** と呼びます。コマンドやオプションは `resarch --help`、`resarch detect --help` で確認できます。

## 使い方

### `sar` と同じ引数で使う

サブコマンドを省略すると、`sar` 互換の引数で保存済みファイルを読み取ります。出力は、入力ファイルの世代によらず sysstat 12.8.0 の書式です。

```bash
resarch -u -f sa01                       # CPU 使用率
resarch -r -f sa01                       # メモリ
resarch -n DEV,EDEV -f sa01              # ネットワークインターフェース
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa01
resarch sar -A -f sa01                   # sar 互換コマンド
resarch sadf -j sa01                     # sadf 互換 JSON
resarch --sar-profile sysstat-10.1.5-el7 -u -f sa01  # RHEL/CentOS 7 の sar と同じ出力
```

本家の癖もそのまま再現します。`-I` には数値を直接渡せず、`-P ALL` と `-P all` は区別され、`-h` はヘルプではなく `--pretty --human` の意味です。`-s` に一致した最初のレコードは差分計算の基準になり、表示されません。対応するオプション、出力プロファイル、`sar` が参照する環境変数は [docs/compatibility.ja.md](docs/compatibility.ja.md) にまとめています。

### sa バイナリを sar テキストへ保存する

```bash
resarch sa2sar sa13 -o sar13             # 全項目・平均・再起動・コメントを保存
resarch sa2sar sa13 --utc -o sar13-utc   # UTC の時刻で保存
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13   # RHEL/CentOS 7 の sa2 が書くテキスト
```

既定は `sar -A -C -t -f sa13` 相当で、採取元のホストが記録した時刻を使います。`-o` を省略するか `-o -` を指定すると標準出力へ書き出します。保存先に既存のファイルがあっても上書きせず、変換に失敗した場合も書きかけのファイルを残しません。

### 独自のサブコマンド

```bash
resarch identify sa01 sa02               # どの sysstat が書いたファイルか
resarch info sa01                        # 世代・ABI・activity 一覧
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # エージェントやパイプラインへ流すとき
resarch summarize sa01 sa02 sa03 --from 09:00 --to 18:00  # 各日のローカル 9〜18 時を集計
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

独自サブコマンドは時刻を実行環境のローカルタイムゾーンで表示し、`--from` / `--to` の `hh:mm[:ss]` も同じタイムゾーンで解釈します。基準は `--timezone <TZ>` (`local` / `utc` / `Asia/Tokyo` のような IANA 名) で変えられます。`summarize` と `compare` の `--from` / `--to` は、集計期間そのものを絞ります。

### 異変の当たりを付ける

```bash
resarch detect sa01                      # いつ・何に異変があったか
resarch detect sa01 --verbose            # 検出ごとの内訳と考えられる解釈まで出す
resarch detect sa13 --svg-dir charts     # 検知した箇所を SVG で保存
```

`resarch detect` は、何を探せばよいか分からなくても、調査の手がかりになる時刻と指標を報告します。値の意味に基づく固定条件、入力ファイルの中央値とばらつき (MAD) からの逸脱、前後の時間窓の水準差を調べ、条件に当たった箇所をエピソードにまとめます。確信度のパーセントは出さず、比較基準そのものが疑わしいときはそう明記し、評価できなかった系列も理由とともに列挙します。報告は日本語と英語で出せます (`--lang`)。

### 対話的に閲覧する (TUI)

```bash
resarch tui sa01
resarch tui sa01 --activity cpu,disk,memory   # activity を絞って開く
```

収録されている activity がタブになり、選んだ item の時系列を表とグラフで読めます。`?` でキー操作の一覧が出ます。`—` は値が無いことを表し、0 ではありません。欠測のところではグラフの線も切れます。

### コマンド一覧

| 目的 | コマンド |
|---|---|
| `sar` 互換の表示 | `resarch sar` (サブコマンド省略可) |
| `sadf` 互換の出力・旧形式の変換 | `resarch sadf` |
| 全項目の `sar` テキストを保存 | `resarch sa2sar` |
| 表や JSON などで閲覧 | `resarch show` |
| 端末で対話的に閲覧 | `resarch tui` |
| 異変を検知 | `resarch detect` |
| 期間を集計 | `resarch summarize` |
| 複数ホストを比較 | `resarch compare` |
| ヘッダと記録項目を確認 | `resarch info` |
| 生成元の sysstat を判別 | `resarch identify` |
| AI エージェント向けスキルを配置 | `resarch skill-install` |

各コマンドの末尾に `--help` を付けるとオプションを確認できます (`sar` 互換コマンドは `resarch sar --help`)。TUI のキー操作、`detect` の報告、SVG グラフ、旧形式の変換など、各コマンドの詳細は [docs/usage.ja.md](docs/usage.ja.md) にあります。

## AI エージェントから使う

`resarch` は自分のコマンドを説明するスキルを同梱しています。エージェントが「いつ使うか」と、それ以上に「**返ってきたものをどう読むか**」を知るためのものです。

```bash
resarch skill-install claude    # ~/.claude/skills/resarch/SKILL.md
resarch skill-install codex     # ~/.codex/skills/resarch/SKILL.md
```

スキル本文はバイナリに埋め込んであるので、リリースバイナリを 1 本置くだけで完結します (チェックアウトもネットワークも要りません)。スキルの大半は `detect` の出力をどう読むかの説明で、エージェントが検知結果から過度な結論を導かないようにしています。**独自出力の空の値は 0 を意味しない**ことや、`--from` / `--to` がサブコマンドごとに何を絞るのかも明記しています。

## 対応形式

登録済みの 28 種類の形式識別値すべてに対応しています。範囲は sysstat 2.2 (`0x015d`) から 12.8 系 (`0x2175`) までで、Red Hat のベンダー派生 `0x1170` や、big-endian・32 bit のホストが書いたファイルも読めます。登録済み全形式への対応と、歴史上の全リリースの検証完了は別の話で、旧形式には項目と ABI の制限があります。詳しくは [docs/formats.ja.md](docs/formats.ja.md) を参照してください。

## 出力の正確さをどう検証しているか

本家 `sysstat` がテストスイートに持っている期待出力を、全件・1 行ずつ reSARch の出力と突き合わせています。対象は 21 ケースです。

| 比較結果 | 件数 |
|---|---:|
| 全文一致 | 16 |
| マスクして一致 | 4 |
| 不一致 | 0 |
| 比較不能 | 1 |

マスクしている 4 件で伏せているのは、`A_DISK` のデバイス名の列だけです。本家はこの名前を、読んだホストの `/dev` / `/sys` で解決するためです。このほか、sysstat 公式リリース 181 版、ディストリビューションのパッケージ 5 種類、CentOS の RPM 11 リリースで `sa` ファイルを採取しました。どれも末尾まで読み切れることを確かめています。詳細と各表記の意味は [docs/verification.ja.md](docs/verification.ja.md) を参照してください。

## 開発

<!-- standard:dev:start -->
[mise](https://mise.jdx.dev/) が必要です。ツールの版は `mise.toml` で固定しています。

```bash
make setup   # ツールチェーン (mise) と依存を取得する
make ci      # CI と同じ検査 (書き換えない)
```

| コマンド | 説明 |
|---|---|
| `make setup` | ツールチェーン (mise) と依存を取得する |
| `make build` | デバッグ版をビルドする |
| `make release` | リリース版をビルドする |
| `make run` | デバッグ版を実行する (引数は ARGS="...") |
| `make test` | テストを実行する |
| `make lint` | clippy を警告ゼロで通す |
| `make fmt` | コードを整形する (書き換える) |
| `make fmt-check` | 整形済みかを確かめる (書き換えない) |
| `make check` | 整形と静的検査 (書き換えない) |
| `make ci` | CI と同じ検査 (書き換えない) |
| `make install` | リリース版を INSTALL_PATH (既定 /usr/local/bin) に入れる |
| `make uninstall` | INSTALL_PATH から取り除く |
| `make clean` | ビルド成果物を消す |

`make` でターゲットの一覧を表示します。リリースは GitHub Actions で行います (**Actions → Release → Run workflow**)。
<!-- standard:dev:end -->

mise を使わない場合は `SYSTEM_TOOLS=1` を付けると PATH 上のツールを使います。その場合、版が CI と一致するとは限りません。本家のデータを使う適合テスト、採取スクリプト、リリースの手順は [docs/development.ja.md](docs/development.ja.md) にあります。

変更履歴は [CHANGELOG.md](CHANGELOG.md) を参照してください。不具合は [GitHub Issues](https://github.com/owayo/re-sar-ch/issues) へ、`resarch --version` の結果、実行コマンド、エラー内容を添えて報告してください。

## ライセンス

<!-- standard:license:start -->
[MIT](LICENSE)
<!-- standard:license:end -->

本家 sysstat は GPL なので、そのテストデータも期待出力もこのリポジトリには同梱していません。`make fixtures` が適合テストのときだけ取得します。`tests/fixtures/` は独自に書き起こしたテストデータで、本体と同じ MIT ライセンスです。実採取した `testdata/sysstat-live/` のファイルの来歴と扱いは[スナップショットの注意事項](testdata/sysstat-live/README.md)を参照してください。
