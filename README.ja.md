<p align="center">
  <img src="docs/images/app.png" width="128" alt="reSARch">
</p>

<h1 align="center">re<strong>SAR</strong>ch</h1>

<p align="center">
  sysstat の sa バイナリを単体で解析する CLI。異変の検知、SVG グラフ、sar / sadf 互換出力を、Linux・macOS・Windows のどれでも実行できます。
</p>

<h3 align="center">対応プラットフォーム</h3>

<p align="center">
  <img src="https://img.shields.io/badge/Linux-FCC624?logo=linux&amp;logoColor=black" alt="Linux">
  <img src="https://img.shields.io/badge/macOS-000000?logo=apple&amp;logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Windows-0078D6" alt="Windows">
</p>

<p align="center">
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/release.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/release.yml/badge.svg?branch=main" alt="Release"></a>
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/owayo/re-sar-ch/releases/latest"><img src="https://img.shields.io/github/v/release/owayo/re-sar-ch" alt="Version"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.ja.md">日本語</a>
</p>

Linux で採取した `sa` ファイルを、手元の PC で調べるためのツールです。
解析する PC に `sar` / `sadf` をインストールする必要はありません。
初めて使う場合は「インストール」と「最初の実行」を読み、詳しい操作や対応形式は目次から参照してください。

## 目次

- [主な機能](#主な機能)
- [動作環境](#動作環境)
- [インストール](#インストール)
- [最初の実行](#最初の実行)
- [使い方](#使い方)
- [コマンド一覧](#コマンド一覧)
- [AI エージェントから使う](#ai-エージェントから使う)
- [対応形式](#対応形式)
- [何のために作ったか](#何のために作ったか)
- [出力の正確さをどう検証しているか](#出力の正確さをどう検証しているか)
- [「sar 互換」の意味](#sar-互換の意味)
- [設計上の要点](#設計上の要点)
- [開発](#開発)
- [リリース](#リリース)
- [ライセンス](#ライセンス)

## 主な機能

- `sar` / `sadf` 互換のテキスト・JSON・CSV・XML 出力
- 端末上の表とグラフで閲覧する TUI
- 異変の検知、期間集計、複数ホストの比較、SVG グラフの保存
- sysstat 2.2 から 12.8 系まで、登録済み 28 形式の読み取り（[旧形式の制限](#centos-345-とそれ以前の形式)あり）
- 各ディストリビューションと sysstat 公式リリースのイメージを使った[自動採取・検証](#sysstat-公式リリースを一括検証する)

## 動作環境

Linux・macOS・Windows で動作します。解析には、Linux 上の sysstat で採取した `sa` ファイルを用意してください。
reSARch 本体は保存済みファイルを解析します。新しく採取する場合は Linux 上の sysstat、
または同梱の[自動採取スクリプト](docs/format/06-live-matrix.md)を使います。
TUI には対話端末、ソースからのビルドには Rust ツールチェーンが必要です。

## インストール

### Homebrew (macOS/Linux)

```bash
brew install owayo/re-sar-ch/re-sar-ch
```

### winget (Windows)

```powershell
winget install owayo.reSARch
```

### ソースからビルドする

Rust ツールチェーンが必要です。

```bash
cargo install --git https://github.com/owayo/re-sar-ch --locked --bin resarch
```

リポジトリには開発用の `xtask` バイナリも入っているため、`--bin resarch` で CLI だけを入れます。

リポジトリを手元でビルドする場合は `make` を使います。
[mise](https://mise.jdx.dev/) が必要で、`mise.toml` で固定した版の Rust ツールチェーンを mise が入れます。
mise を使わない場合は `SYSTEM_TOOLS=1` を付けると、PATH 上の Rust ツールチェーンでビルドします。

```bash
git clone https://github.com/owayo/re-sar-ch.git
cd re-sar-ch
make setup
make install
```

`make install` はリリースビルドを `/usr/local/bin` に入れたあと、
Claude Code と Codex 向けの reSARch スキルを `~/.claude/skills/resarch` と `~/.codex/skills/resarch` に書き出します。
バイナリだけなら `make install-bin` を使います。入れる場所は `INSTALL_PATH`、スキルを入れる先は `SKILL_TARGETS` で変えられます。

```bash
make install INSTALL_PATH="$HOME/.local/bin"   # sudo の要らない場所に入れる
make install SKILL_TARGETS=claude              # Claude Code のスキルだけ
make install SKILL_TARGETS=                    # スキルを入れない
```

Windows では `cargo install --path . --locked --bin resarch` を使います。

### GitHub Releases から取得する

[Releases](https://github.com/owayo/re-sar-ch/releases) から環境別のバイナリを取得できます。

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

> winget でインストールすると、PATH への登録も自動で行われます。インストール後は、変更を反映するために新しいターミナルを開いてください。

## 最初の実行

インストール後、`sa01` を手元のファイルのパスに置き換えて実行します。
Linux 以外で解析する場合は、採取元からファイルをコピーしてください。

```bash
resarch info sa01               # ファイルの形式と記録項目を確認
resarch -u -f sa01              # CPU 使用率を表示
resarch detect sa01             # 調査する時刻と指標の候補を表示
```

記録された統計の種類（CPU、メモリなど）を **activity**、CPU やデバイスなど個々の対象を **item** と呼びます。
コマンドやオプションは `resarch --help`、`resarch detect --help` で確認できます。

## 使い方

### `sar` と同じ引数で使う

サブコマンドを省略すると、`sar` 互換の引数で保存済みファイルを読み取ります。
既定の出力は sysstat 12.8.0 の書式です。入力ファイルの世代によって自動で切り替わることはありません。
対応するオプションと出力プロファイルは[「sar 互換」の意味](#sar-互換の意味)を参照してください。

```bash
resarch -u -f sa01                       # CPU 使用率
resarch -r -f sa01                       # メモリ
resarch -n DEV,EDEV -f sa01              # ネットワークインターフェース
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa01
resarch -u -i 600 -f sa01                # 10 分刻みに間引く
resarch -I --int=0,LOC -f sa01           # 割り込みを番号か名前で選ぶ
resarch sar -A -f sa01                   # sar 互換コマンド
resarch sadf -j sa01                     # sadf 互換 JSON
resarch sadf -g sa01 -- -u -P ALL > cpu.svg  # SVG グラフ (reSARch 独自描画)
resarch --sar-profile sysstat-10.1.5-el7 -u -f sa01  # RHEL/CentOS 7 の sar と同じ出力
```

既定のプロファイルでは、`-I` に数値を直接渡せません。割り込みは `--int` で選びます。
`-P ALL` と `-P all` は区別され、`-h` は `--pretty --human` と同じ意味です。
ヘルプには `--help` を使ってください。`-s` に一致した最初のレコードは差分計算の基準になり、表示されません。

### sa バイナリを sar テキストへ保存する

```bash
resarch sa2sar sa13 -o sar13             # 全項目・平均・再起動・コメントを保存
resarch sa2sar sa13 --utc -o sar13-utc   # UTC の時刻で保存
resarch sa2sar sa13                     # 標準出力へ (パイプでも使える)
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13   # RHEL/CentOS 7 の sa2 が書くテキスト
```

既定は `sar -A -C -t -f sa13` 相当で、採取元のホストが記録した時刻を使います。
`-o` を省略するか `-o -` を指定すると標準出力へ書き出します。保存先に既存の
ファイルがあっても上書きせず、変換に失敗した場合も書きかけのファイルを残しません。

`--sar-profile sysstat-10.1.5-el7` を付けると、RHEL / CentOS 7 のホストの `sa2` が
書くとおりのテキストになります。列の構成も計算も平均の丸めも、Red Hat が配布する
sysstat 10.1.5 に従います。`sa2` がまだ書いていない日の `sarDD` を後から生成できます。
対象の RPM 版、ページサイズ、デバイス名など、再現には条件があります。
詳しくは [RHEL / CentOS 7 の `sar` を再現する](#rhel--centos-7-の-sar-を再現する) を参照してください。
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
resarch detect sa01 --verbose            # 検出ごとの内訳と考えられる解釈まで出す
resarch detect sa01 --lang en            # 英語で出す (下記「出力の言語」)
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
同じ現地時刻が 2 度現れたとき区別できないためです)。

機械可読形式 (json / ndjson / csv) の `start_epoch` / `end_epoch` は epoch 秒のままで、
`--timezone` では変わりません。`detect --format json` / `--format ndjson` だけが
`report_timezone` を持ち、`--from` / `--to` の解釈に使ったタイムゾーンを示します。

`info` / `identify` はファイルヘッダの値をそのまま表示するため、このオプションを持ちません。
互換コマンド (`resarch sar` / `resarch sadf` / `resarch sa2sar`) も本家 sysstat の規則のままで、
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

文字キーは小文字で入力します。主な操作は次のとおりです。

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

メモリなど列が多い activity でも値を読めるように、表示する列を次のように絞ります。

- **全時刻で 1 度も値が出なかった列は既定で外します。** その世代に無いフィールドや
  未実装の列は全行が `—` になり、読みたい列を画面の外へ押し出すだけだからです。
- **画面幅に入らない列は表示範囲から外します。** `shift` + `←` / `→` で横に送れば
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
`v` を押せば高さ 18 行以上の端末で表示できます。18 行未満では表示できません。

値と時刻は、他の独自出力と同じ規則で表示します。

- **`—` は値が無いことで、0 ではありません。** その世代に無いフィールド、
  欠測、差分が取れない区間を、ゼロで埋めません。
- **時刻の前の `!` は不連続**、`R` は再起動、`C` はコメントです。
  採取と採取の間に何が起きたかは観測されていないので、点を線で結びません。
- **グラフの線も同じ理由で切れます。** 欠測と不連続のところで区間を分け、
  飛び越えて結びません。欠測を 0 の点として打つこともしません。
  切れ目があるときは「切れ目 N」と語で書きます (色だけに頼りません)。
  1 点も描けないときは、理由 (`unsupported_by_source: 120` など) を表示します。
- 時刻は `--timezone` の基準 (既定は実行環境のローカル) で、画面にもそう明記します。

TUI は対話端末でのみ動きます。パイプやファイルへ出す場合は `resarch show` /
`resarch sar` を使ってください (その旨のエラーを返します)。

### 異変の当たりを付ける

`resarch detect` は、調査の手がかりになる時刻と指標を報告します。
評価できる各系列に対して、値の意味に基づく固定条件、入力ファイルの中央値と
ばらつき（MAD）からの逸脱、前後の時間窓の水準差を調べます。
条件に当たった箇所は、時間的に近いものをまとめて「エピソード」として出力します。

報告を読むときは、次の点を確認してください。

- **調査優先度と根拠の量は別です。** 優先度は調べる順序の目安です。
  1 ホストの 144 サンプルから較正された確率は求められないため、確信度のパーセントは出しません。
  判断を裏付けたサンプル数を別に示します。
- 比較基準は入力ファイルから作るため、異変が大半を占めると基準も影響を受けます。
  中央値自体が固定条件を満たしている場合は、その系列の逸脱判定が信頼できないことを明記します。
- `MAD = 0` の系列は、ばらつきを測れない状態として報告します。
  微小な値（ε）で割ってスコアを算出することはありません。
- たとえば、20 分にわたる 3 回の採取で高値を観測した場合は、採取回数と期間を報告します。
  採取間の状態は観測していないため、「20 分間高止まり」とは判断できません。
- 評価できなかった系列は、理由とともに列挙します。「異変なし」とは区別してください。

既定のテキスト出力は、エピソードを系列ごとにまとめた要約です。
実際のファイルでは 23 系列から 111 エピソードが生成され、個別に表示すると 1400 行になりました。
どの系列に異変があったかを先に把握できるよう、指標名・件数・最高優先度を見出しにし、
その下に各エピソードの時刻・優先度・採取回数を表示します。

同時に別の系列でも異変を検知した場合は、`+N 系列` と表示します。
比較基準の出所、評価できた範囲、系列ごとの注意点も要約に含めます。
検知結果だけでは確かめられないことは、報告の末尾に指標ごとに 1 度記載します。

要約では、検出ごとの内訳（観測値、比較基準の数値、時間窓の幅）と、
検出パターンに応じた「考えられる解釈」を省略します。
`--verbose` を付けると、各エピソードの詳細を全文表示します。
`--format json` / `--format ndjson` は、`--verbose` の指定によらず全フィールドを出力します。

### 出力の言語

`detect` は日本語と英語で出せます。言語は次の順で決まります。

1. `--lang ja|en`
2. `RESARCH_LANG`
3. `LC_ALL` → `LC_MESSAGES` → `LANG` (`C` / `POSIX` / `C.UTF-8` は英語で確定)
4. `LANGUAGE` (GNU の候補リスト。対応している先頭を採る)
5. ローカルタイムゾーン (`Asia/Tokyo` なら日本語)
6. 英語

言語の選択では、タイムゾーンよりロケールの指定を優先します。
たとえば `Asia/Tokyo` でも、`LANG=en_US.UTF-8` なら英語で出力します。
`LC_ALL=C` も `LANGUAGE` より優先され、英語になります。
スクリプトで出力の言語を固定するために使う指定だからです。

JSON / NDJSON のキーと列挙値は言語によらず英語のままです。言語で変わるのは
人が読む文だけです。

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

## コマンド一覧

| 目的 | コマンド |
|---|---|
| `sar` 互換の表示 | `resarch sar`（サブコマンド省略可） |
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

`show` や `detect` などの独自コマンドは、末尾に `--help` を付けるとオプションを確認できます。
`sar` は `resarch sar --help`、`sadf` は[使い方](#使い方)と[互換出力の仕様](docs/format/03-output-format.md)を参照してください。
互換出力の制限や環境変数は[「sar 互換」の意味](#sar-互換の意味)に記載しています。

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

スキルでは、検知結果から過度な結論を導かないよう、`detect` の出力と注意点の読み方を
説明しています。**独自出力の空の値は 0 を意味しない**ことや、
`--from` / `--to` がサブコマンドごとに何を絞るのかも明記しています。

## 対応形式

登録済みの28種類の形式識別値に対応しています。旧形式の読み取り制限と、走査・出力の検証範囲は後述します。

| `format_magic` | sysstat バージョン | 特徴 |
|---|---|---|
| `0x015d` | 2.2 | 選択した統計を詰めて記録する形式。[制限と ABI](docs/format/08-packed-legacy.md) |
| `0x115a`、`0x215a`〜`0x216f`（欠番 `0x215c` を除く22形式） | 3.2.4 〜 8.1.2 | モノリシック形式。CentOS 3／4／5 と、6.1.1〜6.1.2 の `0x2168` を含む。[世代別の根拠](docs/format/09-legacy-generations.md) |
| `0x2170` | 8.1.3 〜 9.1.5 | activity 配列を導入した世代。本家自身も変換に対応していない |
| `0x1170` | 9.0.4 (RHEL/CentOS 6.5 以降) | **ベンダー派生**。Red Hat が本家のどのリリースでも使われていない値へ magic を振り直したもの。`stats_io` が 20 バイトではなく 80 バイト |
| `0x2171` | 9.1.6 〜 10.2 | file magic 8 バイト、RESTART にペイロードなし |
| `0x2173` | 10.3 〜 11.6 | RESTART レコードの後に volatile activity リストが続き、以降の item 数が変わる |
| `0x2175` | 11.7 〜 12.8 | 自己記述レイアウト、`extra_desc` チェーン、内部に 3 変種 |

`0x1170` は本家のどのリリースにも存在しません。Red Hat が RHEL 6.3 で `stats_io` の
ディスク上のレイアウトを変えたのにフォーマット版を上げなかったため、新しい `sar` が
古いファイルを黙って誤読する状態になり、その修正として RHEL 6.5 で magic を
振り直した経緯があります。本家に 80 バイトの `stats_io` は存在しないので、
申告された item サイズだけで派生を判別できます。

### CentOS 3／4／5 と、それ以前の形式

sysstat 3.2.4 〜 8.1.2 (`0x115a` 〜 `0x216f`) は構造が根本的に違います。
`file_activity[]` の配列も `record_header` もなく、さらに `0x216e` 以前は
ファイル先頭に `file_magic` すらありません。
その世代では magic は `file_hdr` の**内側**にあり、**その位置が世代で動きます** (4 → 36 → 32)。
「先頭 2 バイトを読む」ではこれらのファイルを識別できません。

`0x216f` (8.1.1 / 8.1.2) は**中間世代**です。ファイル先頭に `file_magic` を持つ一方、
本体はまだ旧形式で、`file_activity[]` への転換は次の `0x2170` からになります。

これら22形式を直接読み取れます。CentOS 3.9／4.9 の `0x2163`、5.11 の `0x2169`、
sysstat 6.1.1〜6.1.2 の `0x2168` も含みます。各形式のソースから配置を実測し、
24 リリースの実採取ファイルで `exact=true` と描画成功を確認しています。
32 / 64 bit・両エンディアンの値、配列境界、切り詰めは独立 fixture でも検証します。

CPU、プロセス、メモリ、ネットワークなど、各世代に記録された対応フィールドを読みます。
CPU 別 IRQ の旧配列は境界を検証して読み飛ばし、PID 統計付きパイプ入力は拒否します。
現行版の列と計算規則で表示するため、**当時の `sar` との全文一致を意味しません**。
単位を確定できない値や未記録フィールドは、独自出力では取得不可として扱います。

long 幅を記録しない `0x2167` 以前では既定 8 バイトを仮定し、診断を出します。
ライブラリでは `OpenOptions.legacy_long_bytes = 4` で 32 bit を指定できます。
CLI にはこの指定を切り替えるオプションがないため、long 幅未記録の32 bit入力はライブラリで明示指定してください。
`0x115a` は epoch とタイムゾーンを持たず、記録された日付・時刻を UTC と仮定します。
`0x216e` 以前には正確な sysstat バージョンとアーキテクチャもありません。
詳細は [旧形式の仕様と検証](docs/format/09-legacy-generations.md) を参照してください。

さらに sysstat 2.2 の `0x015d` も読み取れます。
この形式は値のエンディアンを保存しないため、既定 little-endian と診断し、
ライブラリの `OpenOptions.legacy_endian` で明示できます。
1.x・3.0〜3.1 などソース未入手の版は、隣接版と同じ形式だとは推定せず、対応を保証しません。
**登録済み全形式への対応と、歴史上の全リリースの検証完了は別です。**

### activity と ABI の対応範囲

ABI は、整数の幅やバイト順など、採取元の環境でデータを配置する規則を指します。
次の項目にも対応しています。旧形式では、前節の制限が適用されます。

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
| 表示検証 | 本家sysstatのテストデータと回帰テストで、一部の世代・ABI・activity を検証。sadf の期待出力との比較は FAN/IN/TEMP が対象で、43 種すべての全文比較ではない |

revision ごとの定義は [activity 仕様](docs/format/02-activities.md) を参照してください。

## 何のために作ったか

`sar` のログ (`/var/log/sa/saXX`) は、C 構造体をそのままディスクに書き出した形式です。
書き込みは速い反面、読むのが面倒で、通常は**互換性のある世代・アーキテクチャの sysstat** が必要になります。
たとえば、古い 32 bit の PowerPC 機で採取したログは、手元の `sar` で開けない場合があります。

reSARch は対応形式のバイト列を直接読みます。
構造体の配置を**ファイルを書いたホストの ABI** に合わせて解釈するため、
macOS や Windows でも Linux のログを読めます。

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
一致させることは互換性の範囲に含めていないため、SVG の期待出力 1 件は比較対象外として
明示しています。
グラフの値・選択・XML エスケープ・欠測や再起動での線の切断は別の回帰テストで検証します。

不一致は 1 件でもテストを失敗させます。「実装が面倒」「値が合わない」は
マスクの理由として認めていません。

### 検証結果の読み方

「確認済み」だけでは確認の範囲が分からないため、次の項目を区別して記載します。

| 表記 | 確認したこと | この結果だけでは確認できないこと |
|---|---|---|
| 末尾までの走査成功（`exact=true`、測定表では `scan_exact=true`） | 途中終了や不完全なレコードがなく、走査終了位置がファイルサイズと一致した | 全項目のデコード、計算値やテキストの一致 |
| 全 activity のデコード成功（`activity_decode=complete`） | ファイルに宣言された全 activity のデコード計画を作成し、全サンプルを読み飛ばしなしでデコードできた | 元ファイルにないフィールドや単位不明の値の取得、当時の `sar` との出力一致 |
| 本家での再読込一致 | 同じ版の `sar` で保存済みの `sa` を読み直し、採取時のテキストと一致した | reSARch と本家の出力一致 |
| 全文一致／マスクして一致 | reSARch と本家の出力を比較し、全文または明記した列を除く部分が一致した | 比較対象外の版・項目での一致 |

### sysstat 公式リリースを一括検証する

```bash
make sar-upstream-all
# <再開する実行ディレクトリ> を保存済みのディレクトリ名に置き換える
# 途中から再開し、保存済みファイルも現在の reSARch で再検証
SAR_MATRIX_OUTPUT="target/sar-matrix/<再開する実行ディレクトリ>" SAR_MATRIX_RESUME=1 make sar-upstream-all
```

[固定 manifest](tools/sar-matrix/upstream.tsv) に載る181ケースを実行します。
公式 Git の133バージョンと、Git以前に配布されたソースを回収した48バージョンが対象です。
各版のイメージを作成し、採取・同版の `sar` による描画・reSARch の末尾走査と描画まで自動で行います。
ソースのコミットと SHA-256 を固定し、`sa`、本家テキスト、来歴、ハッシュ、差分、ケース別ログを保存します。
失敗した版があっても残りを続け、最後に非ゼロで終了します。結果は `SUMMARY.tsv` で確認できます。

2026-09-24 に **181 / 181 ケースで採取・本家の再読込・reSARch の末尾一致と描画に成功**しました。
[全件の測定結果](docs/measurements/upstream-matrix-2026-09-24.tsv) と
[`sa` / 本家テキスト / 来歴 / SHA-256](testdata/sysstat-live/2026-09-24-official-all/) を Git 管理しています。
公式ソースの27形式と、別途採取した CentOS 固有の `0x1170` で、登録済み28形式をカバーします。
`cargo test --test official_snapshots` で収録済み181ファイルの走査・デコードを再検証できます。
全 181 ケースで `activity_decode=complete` も確認しています。
テキスト比較には全ケースで差分があり、当時の `sar` との全文一致はしていません。
走査・デコード・出力比較の違いは[検証結果の読み方](#検証結果の読み方)を参照してください。

新しい公式タグを追加するには `python3 tools/sar-matrix/update-upstream.py` を実行します。
Git 移行前の全リリースを入手できたわけではないため、**181ケースは入手・固定できた版の集合**です。
未入手の版を成功扱いにはしません。必要環境と保存物の詳細は [手順書](docs/format/06-live-matrix.md) を参照してください。

### 各ディストリビューションとsysstat公式リリースの実採取結果 (2026-09-24)

本家sysstatの期待出力との比較テストとは別に、Apple Container 内で `sadc -S XALL` を実行し、
5種類のディストリビューションが配布するパッケージと、sysstat公式リリース4世代で採取しました。
公式リリースは対象のコミットを固定し、ソースからビルドしています。
各ケースとも Linux 6.18.35 / aarch64 / LP64 で 2 サンプルを記録しました。
reSARch がレコード境界をたどり、ファイル末尾まで走査できることを確認しています。

以下の 2 表の `exact=true` は、[末尾までの走査成功](#検証結果の読み方)を表します。
デコードの対応範囲と、本家 `sar` との出力比較は別に記載します。

| 採取元 | sysstat | 形式識別値 | reSARchの末尾までの走査 |
|---|---:|---:|:---:|
| Alpine 3.23 | 12.7.8 | `0x2175` | `exact=true` |
| Debian 13 | 12.7.5 | `0x2175` | `exact=true` |
| Ubuntu 24.04 LTS | 12.6.1 | `0x2175` | `exact=true` |
| Fedora 44 | 12.7.9 | `0x2175` | `exact=true` |
| Rocky Linux 9 | 12.5.4 | `0x2175` | `exact=true` |
| sysstat公式リリース | 10.2.1 | `0x2171` | `exact=true` |
| sysstat公式リリース | 11.6.6 | `0x2173` | `exact=true` |
| sysstat公式リリース | 12.0.6 | `0x2175` | `exact=true` |
| sysstat公式リリース (採取時の最新版) | 12.8.0 | `0x2175` | `exact=true` |

本家sysstat 12.8.0 の `sar` と比較すると、表示値はすべて一致しました。違いはデバイス名だけです。
本家 `sar` が `254:0` / `254:16` を `vda` / `vdb` に変換するのに対し、reSARch は
別のホストでも同じ名前になる `dev254-0` / `dev254-16` を使います。旧版は当時の列構成を使うため、
現在の 12.8.0 描画プロファイルとの比較結果は診断用差分として保存します。

Gitで管理する[採取結果一覧](docs/measurements/sar-matrix-2026-09-24.tsv)には、
採取に使ったコンテナイメージまたはソースのコミット、ABI、レコード数、末尾一致と、ペアを構成する両ファイルの
SHA-256 を記録しています。対応する `sa` バイナリと本家 `sar -A -C -t` テキストは
[testdata/sysstat-live/2026-09-24](testdata/sysstat-live/2026-09-24) に収録しました。
使い捨てコンテナ VM の 2 サンプルだけを含み、ホスト名は `resarch-fixture` に固定しているため、
運用ホストの情報は含みません。用途と来歴は[スナップショットの注意事項](testdata/sysstat-live/README.md)
を参照してください。

```bash
make sar-latest       # 採取対象を固定したsysstat公式リリース (12.8.0)
make sar-matrix       # 各ディストリビューションのパッケージ版
make sar-generations  # 0x2171 / 0x2173 / 0x2175 の各世代
```

保存されるファイルの説明とDockerでの実行方法は
[自動採取の手順書](docs/format/06-live-matrix.md)を参照してください。

### CentOSの過去リリースでの実採取結果 (2026-09-24)

CentOS Vaultに保存されている11リリース向けの公式RPMパッケージでも採取しました。
`sa` バイナリと、そのファイルを同じ版の `sar` で読み出したテキストを
[testdata/sysstat-live/2026-09-24](testdata/sysstat-live/2026-09-24) に収録しました。
全11ケースで、保存した `sa` を本家 `sar` で再度読み出し、収録したテキストと一致することを確認しています。

| 対象のCentOS | sysstatパッケージの版 | 形式識別値 | reSARchの末尾までの走査 |
|---|---|---|---|
| 3.9 | 5.0.5-11.rhel3 | `0x2163` | `exact=true` |
| 4.9 | 5.0.5-27.el4 | `0x2163` | `exact=true` |
| 5.11 | 7.0.2-13.el5 | `0x2169` | `exact=true` |
| 6.0 | 9.0.4-11.el6 | `0x2170` | `exact=true` |
| 6.5 | 9.0.4-22.el6 | `0x1170` | `exact=true` |
| 6.10 | 9.0.4-33.el6_9.1 | `0x1170` | `exact=true` |
| 7.0 | 10.1.5-4.el7 | `0x2171` | `exact=true` |
| 7.5 | 10.1.5-13.el7 | `0x2171` | `exact=true` |
| 7.9 | 10.1.5-20.el7_9 | `0x2171` | `exact=true` |
| 8.0 | 11.7.3-2.el8 | `0x2175` | `exact=true` |
| 8.5 | 11.7.3-6.el8 | `0x2175` | `exact=true` |

Apple Container / Rosetta上のx86_64・LP64、Linux 6.18.35で採取しました。
実行用のコンテナイメージには、3.9 / 4.9ではCentOS 5.11、6.0 / 6.5では6.6、
8.0 / 8.5では8.4.2105を使用しています。
ケース名はRPMの対象版を表し、当時のOS全体・カーネルを再現した結果ではありません。
CentOS 3.9／4.9／5.11 を含む全 11 ケースで、reSARch の走査結果は `exact=true` です。
3.9／4.9／5.11 の CPU 別 IRQ などの制限と当時の `sar` との表示差は、
[旧形式の対応範囲](docs/format/07-legacy-centos.md#対応範囲と表示)を参照してください。
各ペアの `PROVENANCE.json` には、RPMの取得URL・SHA-256、コンテナイメージの識別ハッシュ、
実行環境、採取コマンドを記録しています。
[採取結果一覧](docs/measurements/centos-matrix-2026-09-24.tsv)では、ペアのハッシュと検証状態を確認できます。

```bash
make sar-all                         # 記録済みの全20ケースを取得・採取・検証
make sar-centos                      # CentOSの11ケースだけ
scripts/collect-sar-matrix.sh centos-6.5  # 単体で再採取
```

保存先は実行ごとに新しく作る `target/sar-matrix/<UTCの実行日時>/` です。
各ケースの `sa` / `sar-A.txt` / 来歴 / SHA-256 / ログと、全体の `SUMMARY.tsv` を収集します。
失敗したケースは一覧に残し、残りの採取後に非ゼロ終了します。
必要なコマンド・採取条件・OSとRPMの対応は[自動採取の手順書](docs/format/06-live-matrix.md)を参照してください。

## 「sar 互換」の意味

reSARch の `sar` 互換性は、入力、計算・出力、CLI に分けて示します。

| 軸 | 範囲 |
|---|---|
| **入力互換** | [対応形式](#対応形式)に記載した28種類。旧形式には項目・ABIの制限あり |
| **計算・出力互換** | どのバージョンの `sar` / `sadf` の表示を再現するか |
| **CLI 互換** | どのオプションと呼び出し方を受け付けるか |

`resarch` バイナリ自体にはリアルタイム採取 (`sadc` 相当) の機能はありません。
同梱の自動採取スクリプトは、各イメージ内の本家 `sadc` を実行します。
バイナリの書き出しは、読んだファイルを別世代の配置で書き直す `sadf -c` に限られ、値そのものは変えません。
テキストやグラフは `sa2sar` / `detect --svg-dir` などで保存できます。
また「読むファイルの世代」と「再現する出力の世代」は別の設定です。
既定では読み取り対応済みのどの形式も sysstat 12.8.0 の書式で出し、`--sar-profile` を指定すると
別の版の `sar` の出力を再現します (次節)。

構文としては解析できるものの、まだ動作しないオプションは、実行時に理由を添えて拒否します。

| オプション | 扱い |
|---|---|
| `sar -o` / `--sadc` | 採取は対象外なので明示的にエラー |
| `sadf -l` | PCP は未対応 |
| `sadf -g -O autoscale,packed,customcol` | この 3 つは明示的に拒否。skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday は対応 |
| `sadf -H` と他形式の併用 | 未対応 (`resarch sadf -H <file>` を使う) |

### RHEL / CentOS 7 の `sar` を再現する

```bash
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13
resarch --sar-profile sysstat-10.1.5-el7 -A -f sa13        # sar 互換コマンド (el7 の文法)
resarch --sar-profile sysstat-10.1.5-el7 -R -f sa13 --sar-page-size 65536   # ppc64le のホスト
```

| `--sar-profile` | 再現する `sar` | 読めるファイル |
|---|---|---|
| `current` (= `sysstat-12.8.0`、既定) | 本家 sysstat 12.8.0 | 読み取り対応済みの全形式 |
| `sysstat-10.1.5-el7` | RHEL / CentOS 7 の `sysstat-10.1.5-17.el7` 〜 `-20.el7_9` | `format_magic` 0x2171 だけ (10.1.5 本体と同じ) |

RHEL 7 のホストが書くレポートは、既定の出力と列見出し以外にも違いがあるので、
プロファイルはその `sar` をまるごと再現します。

- **列**: `-B` の末尾が `%vmeff`、`-d` が `rd_sec/s … svctm`、`-A` が `-R`
  (`frmpg/s bufpg/s campg/s`) を含む、`-r` が 10 列、`-n DEV` に `%ifutil` が無い
- **計算**: CPU `all` 行は個別 CPU を足し直さず、ファイルの集約スロットを
  レコードヘッダの uptime で割る。オフライン CPU は `0.00` を出して値を持ち越す。
  平均の一部は整数で割ってから浮動小数にする (`kbswpcad` の平均 15.5 は `16` でなく `15`)
- **体裁**: `LINUX RESTART` 行に CPU 数が付かない、`COM` 行の本文をそのまま出す。
  Red Hat が `NR_CPUS` を 8192 に上げているので、`-A` では 11 サンプルごとに見出しが出る
- **文法** (`sar` 互換コマンド): `-h` はヘルプ、`-R` がある、`-I` は `XALL` と割り込み番号を
  取る。10.1.5 に無いオプションは usage エラー

プロファイルを自動で選ぶことはしません。ファイルのヘッダに記録されているのは
それを書いた `sadc` の版で、`sa2` が実行した `sar` の版やパッチまでは分からないため、
推測で出力を切り替えると知らないうちにレポートが変わってしまいます。
ファイルから決められない入力も 2 つあります。`-R` の kB → ページ換算は `sar` を
実行したホストのページサイズを使います (`--sar-page-size`、既定 4096 = x86_64 の値)。
`-p` / `-j` もそのホストでデバイス名を引きますが、reSARch は他ホストのログでは引かず
`dev<major>-<minor>` のままにします。
`sadf` はプロファイルを持たず、指定するとエラーにします。

検証は、RHEL 7 ホストが自分で書いたレポート (`sa` バイナリ 29 日分と、その `sa2` が
書いた `sarDD`) とのバイト単位の一致に加え、CentOS のソース RPM からビルドした el7 の
`sar` との突き合わせで行っています。後者は実データと、全 activity と端のケース
(オフライン CPU、カウンタの巻き戻り、インターフェース・ディスクの付け外し、
再起動、コメント) を含む合成データの両方です。規則と検証手順は
[`docs/format/05-sysstat-10.1.5-el7.md`](docs/format/05-sysstat-10.1.5-el7.md) にあります。

### `sar` が参照する環境変数

本家 `sar` は書式の一部を環境から決めます。reSARch も互換出力
(`sar` / `sa2sar` / `show --format sar`) で同じ変数を読みます。

| 変数 | 受け付ける値 | 効果 |
|---|---|---|
| `S_TIME_FORMAT` | `ISO` と完全一致 | バナー行の日付が `MM/DD/YY` から `YYYY-MM-DD` になる |
| `S_REPEAT_HEADER` | 全桁が数字で `> 0` | N 行ごとに列見出しを出し直す — **標準出力が端末でないときだけ** |

一致条件は本家と同じ厳しさです。`S_TIME_FORMAT=iso` は効きませんし、
符号・空白・数字以外を含む `S_REPEAT_HEADER` はエラーではなく無視されます。

標準出力が**端末のとき**は、再表示の間隔をウィンドウの高さ (`rows - 2`) から取り、
`S_REPEAT_HEADER` は見ません (本家の `else if` と同じ)。
どちらも得られなければ間隔は 86400 行です。

この 86400 行は見た目ほど大きくありません。本家は CPU ビットマップを使う activity で
1 サンプルを `count_bits(cpu_bitmap)` 行として数え、`-A` と `-P ALL` はビットマップ全体を
埋めるので、**実 CPU 数に関係なく 1 サンプル = 8200 行**になります。
そのため `sar -A` はパイプ出力でも 11 サンプルごとに CPU の見出しを出し直します。
reSARch もこれを再現します。

これらは `sadf` には効きません。本家が `sadf` で `S_F_PREFD_TIME_OUTPUT` を立てないため、
日付は常に `%Y-%m-%d`、時刻は常に `%H:%M:%S` です。
`--sar-profile sysstat-10.1.5-el7` では `S_REPEAT_HEADER` も読みません
(10.1.5 より後の版で入った変数のため)。`S_TIME_FORMAT` は上のとおり効きます。

## 設計上の要点

実装して初めて分かったことを [`docs/design.md`](docs/design.md) に記録しています。

- 自己記述形式の統計領域では、`unsigned long` は **8 バイトのスロット**を占め、
  32 bit で採取されたファイルでは先頭 4 バイトだけが値を持ちます。
  旧形式の幅と配置は世代ごとに異なります（[2.2 の形式](docs/format/08-packed-legacy.md)、[3.2.4〜8.1.2 の形式](docs/format/09-legacy-generations.md)）。
- 1 item のストライドは**常に `file_activity.size` の申告値**。構造体定義から計算しては
  いけない (旧版の `A_HUGE` は自分のレイアウトと食い違う値を書いている)。
- `0x2175` の 2 つの変種は `header_size` が同じ 328 のまま中身だけが違う。
  判別する手段は、型別フィールド数を見ることしかない。
- **欠落とゼロは別**。採取に使った版がそのフィールドを書いていない場合は `0` ではなく
  `UnsupportedBySource` として扱うので、平均や閾値判定が静かにずれることがない。

フォーマットの完全な仕様は [`docs/format/`](docs/format/) にあります。

## 開発

[mise](https://mise.jdx.dev/) が必要です。ツールの版は `mise.toml` で固定しています。
`make` の各ターゲットは `mise exec` 経由でツールを呼ぶので、シェルで mise を有効にしていなくても固定した版で動きます。
mise を使わない場合は `SYSTEM_TOOLS=1` を付けると PATH 上のツールを使います。その場合、版が CI と一致するとは限りません。

```bash
make setup   # ツールチェーンの導入 (mise install) と依存の取得
make ci      # CI の Test ジョブと同じ検査
```

引数なしの `make` でターゲットの一覧が出ます。次の表の説明は `make help` の出力そのままです。

| コマンド | 説明 |
|---|---|
| `make setup` | Install the toolchain (mise.toml) and fetch dependencies |
| `make build` | Build debug version |
| `make release` | Build release version |
| `make run` | Run the debug build (pass arguments with ARGS="...") |
| `make install` | Build release, install binary, and install skills (claude + codex) |
| `make install-bin` | Build release and install the binary only (no skills) |
| `make skill-install` | Install the AI agent skill from the installed binary (claude + codex) |
| `make uninstall` | Remove the installed binary and the installed skills |
| `make test` | Run tests |
| `make lint` | Run clippy with warnings as errors |
| `make fmt` | Format code |
| `make fmt-check` | Check formatting (no rewrite) |
| `make check` | Run fmt check, clippy, and check (no rewrite) |
| `make ci` | Run the same checks as the CI Test job (no rewrite) |
| `make fixtures` | Fetch upstream sysstat test data used by golden tests (not bundled: GPL) |
| `make conformance` | Run conformance tests against upstream sysstat data (fetches fixtures) |
| `make bench` | Run benchmarks (set RESARCH_BENCH_FILE, or run make fixtures first) |
| `make sar-latest` | Collect and compare an sa file with the latest upstream sysstat |
| `make sar-matrix` | Collect sa files from multiple Linux distribution packages |
| `make sar-generations` | Collect sa files across upstream sysstat format generations |
| `make sar-centos` | Collect eleven CentOS Vault RPM releases, including 6.5 and 7.5 |
| `make sar-all` | Collect all twenty recorded distribution and upstream cases |
| `make sar-upstream-all` | Build and verify every pinned official sysstat source release |
| `make clean` | Clean build artifacts |
| `make help` | Show this help message |

`make conformance` は `make fixtures` で本家のデータを取得してから、`make test` では飛ばす適合テストを回します。
`xmllint` が必要です。macOS には標準で入っており、Debian / Ubuntu では `libxml2-utils` パッケージに含まれます。
sysstat 本体の `sar` の版を記録するテストは、`sar` が無い環境ではスキップします。
CI は Ubuntu 24.04 に sysstat を入れ、比較の基準となる版を固定したうえでジョブのサマリーに記録しています。

`sar-*` のターゲットは、コンテナの中で実際の `sa` ファイルを採取して検証します
([出力の正確さをどう検証しているか](#出力の正確さをどう検証しているか)を参照)。
Apple container か Docker に加えて、ホスト側に `jq`・`curl`・`shasum` が必要です。

本家 sysstat は GPL なので、**そのテストデータも期待出力もこのリポジトリには同梱していません**。
`make fixtures` が固定タグから SHA-256 検証つきで `target/fixtures/` へ取得し、
適合テストのときだけ使います。`tests/fixtures/` は独自に書き起こしたテストデータで、
本体と同じ MIT ライセンスです。実採取した `testdata/sysstat-live/` のファイルとは区別しています。
実採取データの来歴と扱いは[スナップショットの注意事項](testdata/sysstat-live/README.md)を参照してください。

reSARch は `sysstat` のソースを移植したものではなく、ディスク上のフォーマットから
独立に実装したものです。

変更履歴は [CHANGELOG.md](CHANGELOG.md) を参照してください。
不具合は [GitHub Issues](https://github.com/owayo/re-sar-ch/issues) へ、
`resarch --version` の結果、実行コマンド、エラー内容を添えて報告してください。

## リリース

GitHub Actions の **Actions > Release > Run workflow** から出します。
workflow は `Cargo.toml` と `Cargo.lock` の版を上げてタグを打ち、
[GitHub Releases から取得する](#github-releases-から取得する)に挙げた 6 種類のバイナリをビルドして GitHub Release を公開します。
続けて同じ workflow が Homebrew の tap を更新し、winget のマニフェストを提出します。

- **dry_run** をオンにすると、次の版を計算するだけで終わります。コミット、タグ、ビルド、公開はしません。
- 版は CalVer の `YY.M.PATCH` (例: `26.9.106`) で、`PATCH` は毎月 100 から始まります。
  番号からは互換性を壊す変更かどうかが読み取れないので、更新する前に [CHANGELOG.md](CHANGELOG.md) の各版の先頭にある「⚠️ 破壊的変更」を確認してください。

## ライセンス

MIT — [LICENSE](LICENSE) を参照してください。
