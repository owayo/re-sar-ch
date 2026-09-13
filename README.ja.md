<p align="center">
  <img src="docs/images/app.png" width="128" alt="reSARch">
</p>

<h1 align="center">re<strong>SAR</strong>ch</h1>

<p align="center">
  sysstat の sa バイナリを単体で解析する CLI。異変の検知・SVG グラフ・sar / sadf 互換出力を、Linux・macOS・Windows で。
</p>

<h3 align="center">Supported Platforms</h3>

<p align="center">
  <img src="https://img.shields.io/badge/Linux-FCC624?logo=linux&amp;logoColor=black" alt="Linux">
  <img src="https://img.shields.io/badge/macOS-000000?logo=apple&amp;logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Windows-0078D6" alt="Windows">
  <br>
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.ja.md">日本語</a>
</p>

## Install

### Homebrew (macOS/Linux)

```bash
brew install owayo/re-sar-ch/re-sar-ch
```

### From Source

Rust ツールチェーンが必要です。

```bash
cargo install --git https://github.com/owayo/re-sar-ch
```

リポジトリを手元でビルドする場合:

```bash
git clone https://github.com/owayo/re-sar-ch.git
cd re-sar-ch
make install
```

`make install` はバイナリと Claude / Codex 向けスキルをインストールします。バイナリだけなら `make install-bin`。Windows では `cargo install --path .` を使います。

### From GitHub Releases

リリース公開後は [Releases](https://github.com/owayo/re-sar-ch/releases) から環境別のバイナリを取得できます。

#### macOS (Apple Silicon)

```bash
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-aarch64-apple-darwin.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

#### macOS (Intel)

```bash
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-x86_64-apple-darwin.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

#### Linux (x86_64)

```bash
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

#### Linux (x86_64, static/musl)

```bash
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-x86_64-unknown-linux-musl.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

#### Linux (ARM64)

```bash
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-aarch64-unknown-linux-gnu.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

#### Windows

[Releases](https://github.com/owayo/re-sar-ch/releases) の `resarch-x86_64-pc-windows-msvc.zip` を展開し、`resarch.exe` のあるディレクトリを PATH に追加します。

## Usage

### `sar` と同じ引数で使う

reSARch は `sar` の引数構文をそのまま受け付けます。サブコマンドを省略すると `sar` として振る舞います。

```bash
resarch -u -f sa01                       # CPU 使用率
resarch -r -f sa01                       # メモリ
resarch -n DEV,EDEV -f sa01              # ネットワークインターフェース
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa01
resarch -u -i 600 -f sa01                # 10 分刻みに間引く
resarch -I --int=0,LOC -f sa01           # 割り込みを番号か名前で選ぶ
resarch sar -A -f sa01                   # 明示的な互換入口
resarch sadf -j sa01                     # sadf 互換 JSON
resarch sadf -g sa01 -- -u -P ALL > cpu.svg  # SVG グラフ (reSARch 独自描画)
```

`sar` の癖も意図的に再現しています。`-I` は数値を取らない、`-P ALL` と `-P all` は別物、
`-h` は help ではなく `--pretty --human`、`-s` に一致した最初のレコードは
表示されず基準値として消費される、といった挙動です。

### sa バイナリを sar テキストへ保存する

```bash
resarch sa2sar sa13 -o sar13             # 全項目・平均・再起動・コメントを保存
resarch sa2sar sa13 --utc -o sar13-utc   # UTC の時刻で保存
resarch sa2sar sa13                     # 標準出力へ (パイプでも使える)
```

既定は `sar -A -C -t -f sa13` 相当で、採取元に記録された時刻を使います。
`-o` を省略、または `-o -` とすると標準出力へ出します。保存先の既存ファイルは
上書きせず、入力が壊れていた場合も途中までのファイルを保存先に残しません。
旧世代や big-endian の入力も直接読めます。CI では CLI が生成したファイルを、
旧世代 3 本・現行・big-endian の計 5 本の本家期待出力と全文比較します
(実行環境で解決できないディスク名だけ既存のマスクを適用)。

### 独自のサブコマンド

```bash
resarch info sa01                        # 世代・ABI・activity 一覧
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # エージェントやパイプラインへ流す用
resarch detect sa01                      # いつ・何に異変があったか
resarch summarize sa01 sa02 --format json
resarch summarize sa01 sa02 sa03 --from 09:00 --to 18:00  # 各日の UTC 9〜18 時を集計
resarch show sa01 --activity irq --irq-cpus --format ndjson  # 割り込みの CPU 別内訳も表示
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

`summarize` / `compare` の `--from` / `--to` は**集計期間そのもの**を絞ります。
範囲外のサンプルは平均・p95・差分合計のどれにも入らず、期間の端点も範囲内だけになります。
`detect` の `--from` / `--to` は意味が違い、報告範囲だけを絞って比較基準の材料は絞りません。
独自コマンドの `hh:mm[:ss]` はすべて **UTC** です。10 桁の epoch 秒も指定できます。
`compare` の JSON は `comparisons` と `skipped_metrics` を持ち、比較できなかった指標と
観測が無いホストを列挙します。割り込み行は `cpu` (`all` または 0 始まりの CPU 番号) を持ち、
CPU 別内訳の無い旧ファイルでは `all` だけを出します。

### 異変の当たりを付ける

`resarch detect` は、**何を見ればいいか分かっていなくても**「いつ・何に異変があったか」を
返します。評価できるすべての系列に 3 つの観点を当てます。意味が確立している値への
固定条件、そのファイル自身の median と MAD からの逸脱、前後の窓の水準差です。
当たったものは時間的に近いものをまとめてエピソードとして出します。

**推測を測定のように見せることはしません。**

- **確信度のパーセントを出さない。** 1 ホストの 144 点から較正された確率は作れません。
  代わりに調査優先度 (順序尺度) と、それを何サンプルが裏付けたかを**別に**返します。
- **自分の物差しが怪しいときはそう言う。** 比較基準は入力自身から作るので、
  異変がファイルの大半を占めれば基準もそちらへ寄ります。中央値そのものが固定条件を
  満たしている場合、その系列の逸脱判定は当てにならないと報告に明記します。
- **`MAD = 0` を ε で割らない。** ほとんど動かない系列は「散らばりが測れない」状態として
  そのまま報告し、巨大なスコアに化けさせません。
- **採取を継続時間に見せかけない。** 高値 3 回は「20 分にわたる 3 回の採取で高値」で、
  「20 分間高止まり」ではありません。採取と採取の間に何が起きたかは観測されていません。
- **「評価できなかった」を「異変なし」にしない。** 評価できなかった系列は理由つきで
  すべて列挙します。

### 検知した箇所を SVG で確認する

```bash
resarch detect sa13 --svg-dir charts
resarch detect sa13 --svg-dir charts-15m --svg-context 15m
resarch detect sa13 sa14 --activity cpu,disk --from 09:00 --to 10:00 \
  --svg-dir charts-window --svg-context 1h --format json > detections.json
```

検知した**ホスト・リソース・指標ごと**に、検知範囲とその前後だけを切り出して SVG を作成します。
前後幅は既定で各 30 分。`300s` / `15m` / `1h` / `0` で変更できます。
同じ指標の表示範囲が重なれば 1 枚にまとめ、離れた検知や別の起動区間は分けます。
グラフは UTC で表示し、`--from` / `--to` の外も入力にあれば前後の文脈に含めます。
グラフ指定で検知条件や比較基準は変わりません。

- SVG：検知範囲・観測値・閾値や比較基準を表示。欠測や不連続を線でつなぎません。
- `index.json`：ファイル名とホスト・リソース・指標・表示期間・検知根拠の対応表。
- `report.json`：通常の JSON 検知レポート。評価できなかった理由も含みます。

保存先は新規ディレクトリを指定します。通常の text / JSON / NDJSON 出力はそのまま標準出力へ出ます。
検知がなければ SVG は作らず、空の一覧と評価レポートを保存します。
`--lenient` の部分結果は非ゼロ終了し、一覧にも `partial` と入力の欠落理由を残します。
入力の大半に及ぶ所見は「背景」として別図にし、局所的な検知の拡大範囲へ混ぜません。
水準変化の境界を確定した発生時刻とは扱いません。

### 旧世代のファイルを変換する

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # 仮定する HZ を上書きする
```

旧世代のヘッダは tick 周波数を持たないため、主要な Linux アーキテクチャと直読経路に
合わせて **USER_HZ=100** を既定にします。これは `/proc/stat` の単位であり、カーネルの
`CONFIG_HZ` とは別です。生成元の値が分かる場合にだけ `-O hz=` で上書きしてください。
suspend や壁時計の飛びから周波数を推定せず、採用値と出所を stderr に報告します。

## AI エージェントから使う

`resarch` は自分のコマンドを説明するスキルを同梱しています。エージェントが
「いつ使うか」と、それ以上に「**返ってきたものをどう読むか**」を知るためのものです。

```bash
resarch skill-install claude    # ~/.claude/skills/resarch/SKILL.md
resarch skill-install codex     # ~/.codex/skills/resarch/SKILL.md
```

`make install` がバイナリと一緒に両方入れます。スキル本文はバイナリに埋め込んであるので、
リリースバイナリを 1 本置くだけで完結します (チェックアウトもネットワークも要りません)。

スキルの記述の大半は `detect` の出力の読み方に割いています。エージェントが最も
言い過ぎやすいのがここで、添えた留保は飾りではなく判断に使う情報だからです。
その上に分析を積むときに一番効く性質 —— **独自出力で値が空なのは 0 ではない** ——
と、`--from` / `--to` がサブコマンドごとに何を絞るのかも明記しています。

## Supported Formats

sysstat の全フォーマット世代。本家のテストデータとバイト単位で突合して検証しています。

| `format_magic` | sysstat バージョン | 特徴 |
|---|---|---|
| `0x2170` | 〜 9.1.5 | 最古の世代。本家自身が変換対象外にしているもの |
| `0x2171` | 9.1.6 〜 10.2 | file magic 8 バイト、RESTART にペイロードなし |
| `0x2173` | 10.3 〜 11.6 | RESTART レコードの後に volatile activity リストが続き、以降の item 数が変わる |
| `0x2175` | 11.7 〜 12.8 | 自己記述レイアウト、`extra_desc` チェーン、内部に 3 変種 |

このほか次にも対応しています。

- **43 activity すべて** — CPU / メモリ / ディスク / IPv4・IPv6 の各プロトコル / PSI /
  電源センサ / ファイルシステム / HugePages / 割り込み など
- **big-endian・32bit で採取されたファイル** — PowerPC のログも x86-64 と同じように開く
- **`sadf -c` で変換されたファイル** — `upgraded` マーカーを読んで報告する。
  変換そのものも reSARch で行える
- **壊れた入力** — 切り詰め、ありえない item 数、サイズとオフセットの矛盾、
  `nr × nr2 × size` の整数オーバーフローを、鵜呑みにせず検出する

対応状況は次の 4 軸で区別します。「43 activity」は登録数を指し、全歴史的 revision の
表示検証が済んでいるという意味ではありません。

| 軸 | 現在の範囲 |
|---|---|
| デコード | 43 activity のレイアウトを定義。未知 revision は診断付きでスキップ |
| 意味モデル | 各 activity の列メタデータに counter / gauge / identity と単位を記述 |
| 派生指標 | レート・割合・行全体の計算を共通の series 層で処理。値が得られない場合は理由を保持 |
| 表示検証 | 本家 corpus と回帰テストで世代・ABI・activity ごとに検証。sadf の golden は FAN/IN/TEMP が対象で、43 種すべての全文比較ではない |

revision ごとの定義は [activity 仕様](docs/format/02-activities.md) を参照してください。

## 何のために作ったか

`sar` のログ (`/var/log/sa/saXX`) は C 構造体をそのままディスクへ書いた形式です。
書くのは速いが読むのが面倒で、通常は**同じ世代・同じアーキテクチャの sysstat** が必要になります。
5 年前に 32bit の PowerPC 機で採取したログを、手元のマシンの `sar` で開くことはできません。

reSARch はバイト列を直接読みます。sysstat が出荷した全フォーマット世代を知っており、
構造体の配置を「実行ホスト」ではなく「**ファイルを書いたホストの ABI**」から解決するため、
Rust が動く環境ならどこでも動きます。macOS や Windows で Linux のログを読むこともできます。

## 出力の正確さをどう検証しているか

本家 `sysstat` がテストスイートに持っている期待出力を**全件・1 行ずつ**突き合わせています。
対象は 21 ケースです。

| 比較結果 | 件数 |
|---|---:|
| 全文一致 | 16 |
| マスクして一致 | 4 |
| 不一致 | 0 |
| 比較不能 | 1 |

マスクしている 4 件が潰しているのは `A_DISK` のデバイス名列だけです。
本家は `major:minor` を**読んだホストの** `/dev` / `/sys` で名前に解決するので、
`sda1` という名前は期待出力を作ったマシンの構成であって、ファイルの中身ではありません。
reSARch は他ホストで採取したログに誤った名前を付けないよう、あえて `dev8-1` のまま出します。

`sadf -g` は共通の計算値を使う reSARch 独自の SVG 描画です。装飾・座標の全文一致を
互換契約に含めないため、SVG の golden 1 件は比較対象外として明示します。
グラフの値・選択・XML エスケープ・欠測や再起動での線の切断は別の回帰テストで検証します。

不一致は 1 件でもテストを失敗させます。「実装が面倒」「値が合わない」は
マスクの理由として認めていません。

## 「sar 互換」の意味

この言葉は 3 つの別のことを指します。reSARch はまとめて名乗らず、個別に宣言します。

| 軸 | 範囲 |
|---|---|
| **入力互換** | どの世代の `sa` ファイルを読めるか — 4 世代すべて |
| **計算・出力互換** | どのバージョンの `sar` / `sadf` の表示を再現するか |
| **CLI 互換** | どのオプションと呼び出し方を受け付けるか |

reSARch は**採取しません**。リアルタイム採取 (`sadc` 相当) は対象外です。
バイナリの書き出しは「読んだファイルを別世代の配置で書き直したもの」(`sadf -c`) に限り、値は変えません。
テキストやグラフは `sa2sar` / `detect --svg-dir` などで保存できます。
また「読むファイルの世代」と「再現する出力の世代」は別の設定で、
v10 のファイルを v12 の `sar` 書式で出すこともその逆もできます。

現時点で受け付けるが動作しないオプションは、実行時に理由を添えて拒否します。

| オプション | 扱い |
|---|---|
| `sar -o` / `--sadc` | 採取は対象外なので明示的にエラー |
| `sadf -l` | PCP は未対応 |
| `sadf -g -O autoscale,packed,customcol` | この 3 指定は明示的に拒否。skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday は対応 |
| `sadf -H` と他形式の併用 | 未対応 (`resarch sadf -H <file>` を使う) |

## 設計上の要点

実装して初めて分かったことを [`docs/design.md`](docs/design.md) に記録しています。

- `unsigned long` はディスク上で**常に 8 バイトのスロット**を占め、32bit で採取された
  ファイルでは先頭 4 バイトだけが意味を持つ。これが構造体サイズがワード幅を跨いで
  一致する理由。
- 1 item のストライドは**常に `file_activity.size` の申告値**。構造体定義から計算しては
  いけない (旧版の `A_HUGE` は自分のレイアウトと食い違う値を書いている)。
- `0x2175` の 2 変種は `header_size` が同じ 328 のまま中身が違う。
  型別フィールド数を見るしか判別する方法がない。
- **欠落とゼロは別**。採取した版がそのフィールドを書いていない場合は `0` ではなく
  `UnsupportedBySource` として扱うので、平均や閾値判定が静かにずれることがない。

フォーマットの完全な仕様は [`docs/format/`](docs/format/) にあります。

## Development

```bash
make build        # デバッグビルド
make test         # 単体テストと統合テスト
make check        # clippy + fmt
make fixtures     # 本家のテストデータを取得 (下記参照)
make release      # 最適化ビルド
make install      # バイナリとエージェント向けスキルを入れる (claude + codex)
make install-bin  # バイナリだけ
make uninstall    # 両方消す
```

本家 sysstat は GPL なので、**そのテストデータも期待出力もこのリポジトリには同梱していません**。
`make fixtures` が固定タグから SHA-256 検証つきで `target/fixtures/` へ取得し、
適合テストにのみ使います。リポジトリに含まれる fixture は自分で書き起こしたもので、
本体と同じ MIT ライセンスです。

reSARch は `sysstat` のソースを移植したものではなく、ディスク上のフォーマットから
独立に実装したものです。

## License

MIT — [LICENSE](LICENSE) を参照してください。
