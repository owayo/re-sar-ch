<p align="center">
  <img src="docs/images/app.png" width="128" alt="reSARch">
</p>

<h1 align="center">re<strong>SAR</strong>ch</h1>

<p align="center">
  sysstat の sa バイナリを単体で解析する CLI。異変の検知、SVG グラフ、sar / sadf 互換出力を、Linux・macOS・Windows のどれでも実行できます。
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

### winget (Windows)

```powershell
winget install owayo.reSARch
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

> `winget install owayo.reSARch` なら PATH への登録まで自動で行われます。手動でのダウンロードが必要なのは、winget を使わない場合だけです。winget でインストールした直後は、PATH の変更を反映させるために新しいターミナルを開いてください。

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
`-h` は help ではなく `--pretty --human` の意味、`-s` に一致した最初のレコードは
表示されずに基準値として消費される、といった具合です。

### sa バイナリを sar テキストへ保存する

```bash
resarch sa2sar sa13 -o sar13             # 全項目・平均・再起動・コメントを保存
resarch sa2sar sa13 --utc -o sar13-utc   # UTC の時刻で保存
resarch sa2sar sa13                     # 標準出力へ (パイプでも使える)
```

既定は `sar -A -C -t -f sa13` 相当で、採取元のホストが記録した時刻を使います。
`-o` を省略するか `-o -` を指定すると標準出力へ書き出します。保存先に既存の
ファイルがあっても上書きせず、変換に失敗した場合も書きかけのファイルを残しません。
旧世代や big-endian の入力もそのまま読めます。CI では CLI が生成したファイルを、
旧世代 3 本・現行 1 本・big-endian 1 本の計 5 本の本家期待出力と全文比較します
(実行ホストで名前を解決できないディスクにだけ、既存のマスクを適用します)。

### 独自のサブコマンド

```bash
resarch tui sa01                         # 対話的に閲覧する (TUI)
resarch identify sa01 sa02               # どの sysstat が書いたファイルか
resarch info sa01                        # 世代・ABI・activity 一覧
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # エージェントやパイプラインへ流すとき
resarch detect sa01                      # いつ・何に異変があったか
resarch summarize sa01 sa02 --format json
resarch summarize sa01 sa02 sa03 --from 09:00 --to 18:00  # 各日のローカル 9〜18 時を集計
resarch show sa01 --activity irq --irq-cpus --format ndjson  # 割り込みの CPU 別内訳も表示
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

`summarize` / `compare` の `--from` / `--to` は**集計期間そのもの**を絞ります。
範囲外のサンプルは平均・p95・差分合計のどれにも入らず、期間の端点も範囲内のサンプルに
合わせて縮まります。`detect` では同じ 2 つのオプションの意味が変わり、報告する範囲だけを
絞って、比較基準を組み立てる材料は絞りません。
独自コマンドの `hh:mm[:ss]` は**時刻表示と同じタイムゾーン**で比較します
(既定は実行環境のローカル)。10 桁の epoch 秒も指定できます。
`compare` の JSON は `comparisons` と `skipped_metrics` を持ち、`skipped_metrics` には
比較できなかった指標と、その指標の観測値が無いホストが並びます。割り込みの行は `cpu`
(`all` または 0 始まりの CPU 番号) を持ち、CPU 別の内訳が無い旧ファイルでは `all` だけを出します。

#### 時刻のタイムゾーン

> **v26.9.102 で既定が変わりました。** 独自サブコマンドの時刻は UTC 固定から
> 実行環境のローカルタイムゾーンになりました。従来どおり UTC で出すには `--utc` を
> 付けてください。詳細は [CHANGELOG.md](CHANGELOG.md) を参照してください。


独自サブコマンド (`show` / `summarize` / `detect` / `compare` / `tui`) は、時刻を
**実行環境のローカルタイムゾーン**で表示します。`--timezone <TZ>` で基準を変えられ、
`local` (既定) / `utc` / `Asia/Tokyo` のような IANA 名を受け付けます。`--utc` は
`--timezone utc` の別名で、`--timezone` との同時指定はエラーです。

```bash
resarch summarize sa01                        # 2026-08-31 15:10:01+09:00
resarch summarize sa01 --utc                  # 2026-08-31 06:10:01Z
resarch summarize sa01 --timezone Asia/Tokyo  # 実行環境によらず日本時間で読む
```

同じ基準を `--from` / `--to` の `hh:mm[:ss]` の解釈にも使うので、画面に出ている 09:00 と
`--from 09:00` が食い違いません。10 桁の epoch 秒はタイムゾーンの影響を受けません。
タイムゾーンは IANA 名 (`Asia/Tokyo`) で表示し、IANA 名を特定できない環境では数値オフセット
(`+09:00`) になります。`JST` のような略称は使いません (重複があるうえ、夏時間の切り替え日に
同じ壁時計が 2 度現れたとき区別できないためです)。

機械可読形式 (json / ndjson / csv) の `start_epoch` / `end_epoch` は epoch 秒のままで、
`--timezone` では変わりません。`detect --format json` / `--format ndjson` だけが
`report_timezone` を持ち、`--from` / `--to` をどの壁時計として読んだかを示します。

`info` / `identify` にはこのオプションがありません (ファイルヘッダに書かれた値を
そのまま出すコマンドで、読み手のタイムゾーンで開き直す対象がないためです)。
互換入口 (`resarch sar` / `resarch sadf` / `resarch sa2sar`) も本家 sysstat の規則のままで、
`--timezone` の影響を受けません。

### 対話的に閲覧する (TUI)

```bash
resarch tui sa01
resarch tui sa01 --activity cpu,disk,memory   # activity を絞って開く
```

収録されている activity がタブになり、選んだ item の時系列を表とグラフで読めます。

```
<host>  Linux 2.6.32-696.1.1.el6.x86_64 / x86_64  (2 CPU)
2026-08-31  sa01  時刻は Asia/Tokyo  (144 サンプル)
 CPU   PCSW   SWAP   PAGE   IO   MEMORY   KTABLES   QUEUE   SERIAL   DISK   NET_DEV   ...
┌ A_CPU / all / idle (percent) ─────────────────────────────────────────────────────┐
│100.00 │      ╭──╮                                                                  │
│       │──────╯  ╰───╮          ╭────────                                           │
│  0.00 │             ╰          ╯            切れ目 1 (不連続・欠測)                │
│       └──────────────────────────────────────────────────────────────────────────  │
│15:10:01                      18:40:01                                   22:10:01   │
└───────────────────────────────────────────────────────────────────────────────────┘
┌ A_CPU — CPU 使用率  [all]  ───────────────────────────────────────────────────────┐
│time       user     nice     system   iowait   steal    idle     usr      ...       │
│ 15:10:01  0.14     0.00     0.12     0.03     0.00     99.71    0.14     ...       │
│ 15:20:01  0.13     0.00     0.09     0.02     0.00     99.75    0.13     ...       │
└───────────────────────────────────────────────────────────────────────────────────┘
┌ A_MEMORY — メモリ・スワップ利用状況  [-]  9-18/23 列 ──────────────────────────────┐
│time       kbactive kbinact kbdirty kbanonpg kbslab kbkstack kbpgtbl kbvmused       │
│ 15:23:05  794624   638144  192.00  111872   234432 2784.0   3200.0  17536.0        │
└───────────────────────────────────────────────────────────────────────────────────┘
←→ activity  ↑↓ 時刻  i item (3)  / 絞り込み  home/end 端  c 列  [] 送り  v グラフ  ? help  q 終了
```

キーはすべて小文字と記号です。`Shift` の有無で別の操作になると、打ち間違いが
「別の機能が動く」形で出てしまうためです。

| キー | 動作 |
|---|---|
| `←` / `→` | activity を切り替える |
| `shift` + `←` / `→` | 表を横に送る |
| `↑` / `↓`、`pgup` / `pgdn` | 時刻を移動する |
| `home` / `end` | 先頭 / 末尾 |
| `i` | item (デバイス・インターフェース・CPU) を選ぶ |
| `/` | item を名前で絞り込む |
| `c` | 列を選ぶ (表に出す列とグラフの列) |
| `[` / `]` | グラフの列を前 / 次へ (表に出ている列だけを送る) |
| `v` | グラフの表示を切り替える |
| `?` | キー操作の一覧 |
| `q`、`ctrl-c` | 終了 |

`c` のポップアップでは `space` でその列を表に出し入れ、`a` で全列と既定を切り替え、
`enter` で確定します。確定するとカーソルの行がグラフに描かれる列になります。
`esc` は取り消しで、表もグラフも元のままです。

列が多い activity (MEMORY は 19 列) では、すべてを並べると 1 列あたり数桁に潰れて
どの値も読めなくなります。そこで表は次の 2 つで列を絞ります。

- **全時刻で 1 度も値が出なかった列は既定で外します。** その世代に無いフィールドや
  未実装の列は全行が `—` になり、読みたい列を画面の外へ押し出すだけだからです。
- **画面幅に入らない列は潰さずに落とします。** `shift` + `←` / `→` で横に送れば
  残りも読めます。タイトルに `9-18/23 列` と位置が出るので、右に続きがあることも
  分かります。表から外している列がある場合は「他 1 列 (c で選ぶ)」も添えます。

`c` を押すと列の一覧が出ます。値が出なかった列も一覧には並び、灰色で
「(全時刻で値なし)」と注記が付きます。列幅は全サンプルの値を見て決めるので、
行をスクロールしても幅が揺れません。

グラフは表の上に出て、選んでいる activity / item / 列の 1 系列を描きます。
**グラフに出している列は、下の表の見出しも線と同じ色**になるので、どの列が描かれて
いるかが一目で分かります。表で選んでいる時刻には縦線が立つので、表とグラフが同じ時点を
指していることも分かります。
端末が低いとき (24 行以下) は既定で出しません。表が数行しか見えないと時刻を追えなくなるためです。
`v` で明示すれば 18 行まで出せますが、それより低いと出せません。

表示の規律は他の出力と同じです。

- **`—` は値が無いことで、0 ではありません。** その世代に無いフィールド、
  欠測、差分が取れない区間を、ゼロで埋めません。
- **時刻の前の `!` は不連続**、`R` は再起動、`C` はコメントです。
  採取と採取の間に何が起きたかは観測されていないので、点を線で結びません。
- **グラフの線も同じ理由で切れます。** 欠測と不連続のところで区間を分け、
  飛び越えて結びません。欠測を 0 の点として打つこともしません。
  切れ目があるときは「切れ目 N」と語で書きます (色だけに頼りません)。
  1 点も描けないときは、空の軸ではなく理由 (`unsupported_by_source: 120` など) を出します。
- 時刻は `--timezone` の基準 (既定は実行環境のローカル) で、画面にもそう明記します。

TUI は対話端末でのみ動きます。パイプやファイルへ出す場合は `resarch show` /
`resarch sar` を使ってください (その旨のエラーを返します)。

### 異変の当たりを付ける

`resarch detect` は、**何を見ればいいか分かっていなくても**「いつ・何に異変があったか」を
返します。評価できる系列すべてを 3 つの観点で見ます。意味が確立している値に対する
固定条件、そのファイル自身の中央値と MAD からの逸脱、そして前後の窓の水準差です。
条件に当たった箇所は、時間的に近いものどうしをまとめてエピソードとして出力します。

**推測を測定のように見せることはしません。**

- **確信度のパーセントを出さない。** 1 ホストぶんの 144 サンプルから、較正された確率は
  作れません。代わりに調査優先度 (順序尺度) と、それを裏付けたサンプル数とを**別々に**返します。
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
グラフの時刻には `--timezone` の基準 (既定は実行環境のローカル) を添えます。
`index.json` の `timezone` は実際に使った基準名です。`--from` / `--to` の範囲外でも、入力にサンプルがあれば
前後の文脈として含めます。グラフ関連のオプションを付けても、検知の閾値や比較基準は
変わりません。

- SVG：検知範囲・観測値・閾値や比較基準を表示。欠測や不連続の箇所では線を切ります。
- `index.json`：ファイル名とホスト・リソース・指標・表示期間・検知根拠の対応表。
- `report.json`：通常の JSON 検知レポート。評価できなかった理由も含みます。

保存先は新規ディレクトリを指定します。通常の text / JSON / NDJSON 出力はそのまま標準出力へ出ます。
検知がなければ SVG は作らず、空の一覧と評価レポートを保存します。
`--lenient` で結果が部分的になった場合は終了コードを非ゼロにし、一覧にも `partial` と
入力の欠落理由を残します。入力の大半に及ぶ所見は「背景」として別の図にまとめ、
局所的な検知の表示範囲を広げないようにします。水準変化の境界は、確定した発生時刻としては
扱いません。

### 旧世代のファイルを変換する

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # 仮定する HZ を上書きする
```

旧世代のヘッダには tick 周波数が入っていないため、主要な Linux アーキテクチャと
直接読み取りの経路に合わせて **USER_HZ=100** を既定にします。これは `/proc/stat` の
単位であり、カーネルの `CONFIG_HZ` とは別のものです。生成元が別の tick 周波数だと
分かっている場合にだけ、`-O hz=` で上書きしてください。suspend や壁時計の飛びから
周波数を推定することはありません。採用した値とその根拠は stderr に報告します。

## AI エージェントから使う

`resarch` は自分のコマンドを説明するスキルを同梱しています。エージェントが
「いつ使うか」と、それ以上に「**返ってきたものをどう読むか**」を知るためのものです。

```bash
resarch skill-install claude    # ~/.claude/skills/resarch/SKILL.md
resarch skill-install codex     # ~/.codex/skills/resarch/SKILL.md
```

`make install` なら、バイナリと一緒に両方インストールされます。スキル本文はバイナリに
埋め込んであるので、リリースバイナリを 1 本置くだけで完結します
(チェックアウトもネットワークも要りません)。

スキルの記述は、その大半を `detect` の出力の読み方に割いています。エージェントが最も
言い過ぎやすいのがここで、添えた留保は飾りではなく判断に使う情報だからです。
この出力の上に分析を積み重ねるときに一番効いてくる性質 ——
**独自出力の空の値は 0 ではない** —— と、`--from` / `--to` がサブコマンドごとに
何を絞るのかも明記しています。

## Supported Formats

sysstat の全フォーマット世代に対応し、本家のテストデータとバイト単位で突合して検証しています。

| `format_magic` | sysstat バージョン | 特徴 |
|---|---|---|
| `0x2170` | 〜 9.1.5 | 読み取りに対応している最古の本家世代。本家自身も変換に対応していない |
| `0x1170` | 9.0.4 (RHEL/CentOS 6.5 以降) | **ベンダー派生**。Red Hat が本家のどのリリースでも使われていない値へ magic を振り直したもの。`stats_io` が 20 バイトではなく 80 バイト |
| `0x2171` | 9.1.6 〜 10.2 | file magic 8 バイト、RESTART にペイロードなし |
| `0x2173` | 10.3 〜 11.6 | RESTART レコードの後に volatile activity リストが続き、以降の item 数が変わる |
| `0x2175` | 11.7 〜 12.8 | 自己記述レイアウト、`extra_desc` チェーン、内部に 3 変種 |

`0x1170` は本家のどのリリースにも存在しません。Red Hat が RHEL 6.3 で `stats_io` の
ディスク上のレイアウトを変えたのにフォーマット版を上げなかったため、新しい `sar` が
古いファイルを黙って誤読する状態になり、その修正として RHEL 6.5 で magic を
振り直した経緯があります。本家に 80 バイトの `stats_io` は存在しないので、
申告された item サイズだけで派生を判別できます。

### さらに古い世代 — 識別のみ (読み取りは未実装)

sysstat 3.2.4 〜 8.1.2 (`0x115a` 〜 `0x216f`) は構造が根本的に違います。
`file_activity[]` の配列も `record_header` もなく、さらに `0x216e` 以前は
ファイル先頭に `file_magic` すらありません。
その世代では magic は `file_hdr` の**内側**にあり、**その位置が世代で動きます** (4 → 36 → 32)。
「先頭 2 バイトを読む」ではこれらのファイルを識別できません。

`0x216f` (8.1.1 / 8.1.2) は**中間世代**です。ファイル先頭に `file_magic` を持つ一方、
本体はまだ旧形式で、`file_activity[]` への転換は次の `0x2170` からになります。

reSARch はこれらを識別し、どの sysstat が書いたファイルかを報告します。

```
sa07: sysstat 6.1.3〜7.0.4 が書いた形式です (format_magic=0x2169)。この世代の読み取りは未実装です
```

これは「sysstat のファイルではない」とも「未対応のフォーマット」とも別の診断です。
3 つを混同すると調査が遠回りになるため、意図的に分けています。
この世代のデコードはまだ実装していません。

このほか、次のものにも対応しています。

- **43 activity すべて** — CPU / メモリ / ディスク / IPv4・IPv6 の各プロトコル / PSI /
  電源センサ / ファイルシステム / HugePages / 割り込み など
- **big-endian・32bit で採取されたファイル** — PowerPC のログも x86-64 と同じように開く
- **`sadf -c` で変換されたファイル** — `upgraded` マーカーを読んで報告する。
  変換そのものも reSARch で行える
- **壊れた入力** — 切り詰め、ありえない item 数、サイズとオフセットの矛盾、
  `nr × nr2 × size` の整数オーバーフローを、鵜呑みにせず検出する

対応状況は次の 4 つの軸に分けて示します。「43 activity」はレイアウトを登録した数であって、
過去のすべての revision について表示まで検証済み、という意味ではありません。

| 軸 | 現在の範囲 |
|---|---|
| デコード | 43 activity のレイアウトを定義。未知 revision は診断付きでスキップ |
| 意味モデル | 各 activity の列メタデータに counter / gauge / identity と単位を記述 |
| 派生指標 | レート・割合・行全体の計算を共通の series 層で処理。値が得られない場合は理由を保持 |
| 表示検証 | 本家の corpus と回帰テストで、一部の世代・ABI・activity を検証。sadf の golden は FAN/IN/TEMP が対象で、43 種すべての全文比較ではない |

revision ごとの定義は [activity 仕様](docs/format/02-activities.md) を参照してください。

## 何のために作ったか

`sar` のログ (`/var/log/sa/saXX`) は、C 構造体をそのままディスクに書き出した形式です。
書き込みは速い反面、読むのが面倒で、通常は**互換性のある世代・アーキテクチャの sysstat** が必要になります。
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

マスクしている 4 件で伏せているのは `A_DISK` のデバイス名の列だけです。
本家は `major:minor` を**読んだホストの** `/dev` / `/sys` で名前に解決するので、
`sda1` という名前は期待出力を作ったマシンの構成であって、ファイルの中身ではありません。
reSARch は他ホストで採取したログに誤った名前を付けないよう、あえて `dev8-1` のまま出します。

`sadf -g` は、共通の計算値を使った reSARch 独自の SVG 描画です。装飾や座標まで
一致させることは互換性の範囲に含めていないため、SVG の golden 1 件は比較対象外として
明示しています。
グラフの値・選択・XML エスケープ・欠測や再起動での線の切断は別の回帰テストで検証します。

不一致は 1 件でもテストを失敗させます。「実装が面倒」「値が合わない」は
マスクの理由として認めていません。

## 「sar 互換」の意味

この言葉は、別々の 3 つのことを指します。reSARch はそれらをまとめて名乗らず、軸ごとに宣言します。

| 軸 | 範囲 |
|---|---|
| **入力互換** | どの世代の `sa` ファイルを読めるか — 4 世代すべて |
| **計算・出力互換** | どのバージョンの `sar` / `sadf` の表示を再現するか |
| **CLI 互換** | どのオプションと呼び出し方を受け付けるか |

reSARch は**採取しません**。リアルタイム採取 (`sadc` 相当) は対象外です。
バイナリの書き出しは、読んだファイルを別世代の配置で書き直す `sadf -c` に限られ、値そのものは変えません。
テキストやグラフは `sa2sar` / `detect --svg-dir` などで保存できます。
また「読むファイルの世代」と「再現する出力の世代」は別の設定で、
v10 のファイルを v12 の `sar` 書式で出すこともその逆もできます。

構文としては解析できるものの、まだ動作しないオプションは、実行時に理由を添えて拒否します。

| オプション | 扱い |
|---|---|
| `sar -o` / `--sadc` | 採取は対象外なので明示的にエラー |
| `sadf -l` | PCP は未対応 |
| `sadf -g -O autoscale,packed,customcol` | この 3 つは明示的に拒否。skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday は対応 |
| `sadf -H` と他形式の併用 | 未対応 (`resarch sadf -H <file>` を使う) |

## 設計上の要点

実装して初めて分かったことを [`docs/design.md`](docs/design.md) に記録しています。

- `unsigned long` はディスク上で**常に 8 バイトのスロット**を占め、32bit で採取された
  ファイルでは先頭 4 バイトだけが意味を持つ。構造体のサイズがワード幅をまたいで
  一致するのは、このためである。
- 1 item のストライドは**常に `file_activity.size` の申告値**。構造体定義から計算しては
  いけない (旧版の `A_HUGE` は自分のレイアウトと食い違う値を書いている)。
- `0x2175` の 2 つの変種は `header_size` が同じ 328 のまま中身だけが違う。
  判別する手段は、型別フィールド数を見ることしかない。
- **欠落とゼロは別**。採取に使った版がそのフィールドを書いていない場合は `0` ではなく
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
適合テストのときだけ使います。リポジトリに含まれる fixture は独自に書き起こしたもので、
本体と同じ MIT ライセンスです。

reSARch は `sysstat` のソースを移植したものではなく、ディスク上のフォーマットから
独立に実装したものです。

## License

MIT — [LICENSE](LICENSE) を参照してください。
