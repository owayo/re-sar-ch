---
name: resarch
description: >-
  sysstat の sa バイナリ (/var/log/sa/saXX) を sar/sadf/libc なしで解析する CLI。
  「いつ・何に異変があったか」を当てる resarch detect、エージェント向け構造化出力の
  show --format ndjson、期間集計の summarize、ホスト比較の compare を提供。
  sar / sadf 互換出力 (-d -p -r -j -x) も本家と全文一致。旧世代 (0x1170 / 0x2170 /
  0x2171 / 0x2173) のファイルも直読でき、32bit / big-endian で採取されたものも読める。
  どのバージョンの sysstat が書いたファイルかは resarch identify が答える
  (読めない最古世代 0x115a〜0x216f も識別できる)。
  sa ファイル・sar ログ・sadf・性能障害の事後調査・リソース異変の切り分け、
  「このファイルはどの sysstat のものか」「sar が読めないと言う」場面で発動。
allowed-tools: Bash(resarch:*)
---

# resarch

sysstat が `/var/log/sa/saXX` に書く**バイナリログを直接読む** CLI。`sar` も `sadf` も
C ライブラリも要らない。`sysstat` が出荷した全フォーマット世代を知っており、構造体の配置を
「実行ホスト」ではなく「**ファイルを書いたホストの ABI**」から解決するので、
macOS や Windows で Linux のログを読める。

## いつ使うか

| やりたいこと | コマンド |
|---|---|
| **障害の事後調査。いつ何が起きたか当たりを付ける** | `resarch detect <file>` |
| 検知リソースごとに前後の推移を SVG にする | `resarch detect <file> --svg-dir charts --svg-context 30m` |
| 構造化データを取り出してエージェント自身で分析する | `resarch show <file> --format ndjson` |
| 期間全体の平均・p95・ボトルネック判定 | `resarch summarize <file> --format json` |
| 複数ホストを同じ時間窓で比べる | `resarch compare --host a=<f1> --host b=<f2>` |
| **どのバージョンの sysstat が書いたファイルか調べる** | `resarch identify <file>...` |
| ファイルの世代・ABI・収録 activity を知る | `resarch info <file>` |
| 既存のツールやスクリプトに食わせる (本家と同じテキスト) | `resarch -u -f <file>` / `resarch sadf -j <file>` |
| 旧世代のファイルを他ツールへ渡せる形に変換する | `resarch sadf -c <file> > out` |
| sa バイナリを全項目の sar テキストへ保存する | `resarch sa2sar sa13 -o sar13` |

`sa2sar` は平均・RESTART・COMMENT も含め、既定は採取元に記録された時刻、
`--utc` で UTC に切り替える。`-o` 省略または `-o -` は標準出力。
保存先は上書きせず、失敗時も途中までのファイルを保存先に残さない。

**`sar` が「読めない」と言ったファイルでも読める。** `sar` は `format_magic` が現行と
違うと即エラーにするが、`resarch` は 5 世代 (`0x1170` / `0x2170` / `0x2171` / `0x2173` /
`0x2175`) を直読する。32bit / big-endian で採取されたファイル (PowerPC など) も同じように開く。

## どのバージョンの sysstat が書いたファイルか調べる

```bash
resarch identify /var/log/sa/sa07
resarch identify sa*.bin --format json      # 機械可読
```

```
FILE                   FORMAT  SYSSTAT                       RECORDED  ENDIAN  READ  NOTE
sa01                   0x1170  9.0.4 (RHEL/CentOS 6.5 以降)  9.0.4     little  yes
sa07                   0x2169  6.1.3〜7.0.4                  -         little  no    この世代の読み取りは未実装
broken.bin             -       -                             -         -       -     sysstat のデータファイルではない
```

読めない世代でも判定結果を返し、**終了コードは 0 のまま**である
(「読めない」ことは失敗ではない)。ファイル自体が開けないときだけ非ゼロになる。
`info` との違いは、`info` がヘッダを解釈できるファイルしか扱えないのに対し、
`identify` は**先頭 1 KiB だけを読んで「何のファイルか」に答える**点にある。

列の意味で取り違えやすいのは次の 2 つである。

- **`SYSSTAT` は magic から分かる範囲**であって、書き手のバージョンそのものではない。
  1 つの magic が複数バージョンに跨る (例: `0x2169` は 6.1.3〜7.0.4 の 4 リリース)。
- **`RECORDED` はファイル自身が記録しているバージョン**。`file_magic` を持つ世代
  (`0x216f` 以降) にしか無いので、それ以前は `-` になる。**「記録が無い」であって
  「読み取れなかった」ではない。**

`NOTE` が「ヘッダの構造が一致しない」なら、magic は既知だがヘッダが壊れているか
途中で切れている。「この format_magic は未知」なら、こちらがまだ知らない世代である。

## まず detect を打つ

「このホストで何かあった」という調査の入口はこれ。**何を見ればいいか分かっていなくてよい。**

```bash
resarch detect /var/log/sa/sa07
resarch detect sa07 --format json          # エージェント向け (型のフィールドをそのまま出す)
resarch detect sa07 --min-priority investigate   # 優先度の下限で絞る
resarch detect sa07 --from 09:00 --to 10:00      # 報告範囲だけを絞る (下記の注意)
```

評価できるすべての系列に 3 つの観点を当て、当たったものを時間的に近いものごとに
**エピソード**としてまとめる。

| 観点 | 何を見るか |
|---|---|
| 絶対水準 | 意味が確立している値への固定条件 (direct reclaim の発生、`%util` の高止まりなど) |
| 参照分布からの逸脱 | そのファイル自身の median と MAD からの偏り |
| 時間的変化 | 前後の窓の水準差 |

### detect の出力を読むときに必ず押さえること

**この出力は「推測を測定のように見せない」ことに全力を使っている。** 添えられた留保は
飾りではなく、そのまま判断に使う情報である。

- **確信度のパーセントは出ない。** 1 ホストの 144 点から較正された確率は作れない。
  代わりに**調査優先度** (順序尺度: 参考 / 注視 / 調査) と**根拠の充足度**が
  **別のフィールド**で出る。2 つを掛け合わせて 1 つのスコアにしないこと
- **`! 比較基準が異変側へ寄っている疑いがある`** が出たら、その系列の**逸脱判定は
  当てにならない**。比較基準はそのファイル自身から作るので、異変がファイルの大半を
  占めていれば基準もそちらへ寄る。`中央値そのものが固定条件を満たしている` と
  書かれていたら、逸脱の数値を根拠にしてはいけない
- **「20 分にわたる 3 回の採取」は「20 分間ずっと」ではない。** `sar` のデータは
  離散的な採取で、採取と採取の間に何が起きたかは観測されていない
- **「観点が 2 つ当たった」は裏付けが 2 倍ではない。** 3 経路は相関する
  (`%idle` が下がれば 3 つとも鳴りやすい)。出力にもそう書いてある
- **`前後で水準が違う境目` は「その時刻に変わった」ではない。** 指しているのは
  採用した前後窓の分割時刻である
- **背景の所見**は「重要でない」ではなく「**いつ起きたかの手がかりを持たない**」。
  入力のほぼ全体を占める状態 (終日続くスワップ使用など) はここに分けられる
- **`評価できなかった系列`** は「異変なし」ではない。理由つきで列挙されるので、
  そこを読まずに「問題なし」と結論しないこと
- **`前後窓を取れない端 N 採取は見ていない`** が出たら、ファイル端で起きた変化は
  この経路では評価されていない

## 検知箇所のグラフ

`detect --svg-dir <新規ディレクトリ>` は、検知したホスト・起動区間・リソース・指標ごとに
前後各30分のSVGを保存する。`--svg-context 300s/15m/1h/0` で幅を変えられる。
重なる表示範囲は同一系列内でまとめ、離れた検知は別SVGにする。
時刻は `--timezone` の基準 (既定はローカル) で表示し、`index.json` の `timezone` に
実際に使った基準名を出す。`--from` / `--to` 外の文脈も入力にあれば残す。検知条件は変えない。
`index.json` がファイルと検知の対応表、`report.json` が評価不能理由も含むレポート。
検知なしはSVGを作らず空の一覧を残す。部分入力は一覧で `partial` と明記し非ゼロ終了。
通常レポートも標準出力へ出る。既存ディレクトリは上書きしない。

## 構造化データを取り出す

エージェント自身で分析するなら NDJSON か JSON。

```bash
resarch show sa07 --format ndjson --activity cpu,disk
resarch show sa07 --format ndjson --values both     # 生カウンタと派生値を別名前空間で
resarch show sa07 --format json --from 09:00 --to 18:00
```

`--values` は `raw` (累積カウンタの生値) / `derived` (レート・割合、既定) / `both`。

### 欠落とゼロは別

**これが `resarch` の最重要の性質。** 独自出力では、値が無いことを理由つきで返す。

| 品質 | 意味 |
|---|---|
| `unsupported_by_source` | **その世代のファイルにフィールドが無い。** 0 ではない |
| `missing_in_sample` | フィールドはあるが、そのレコードで取得できていない |
| 不連続 (`restart` / `item_replaced` / …) | 差分が作れない。レートを計算していない |

`0` として扱うと平均・p95・閾値判定が静かに誤る。**独自出力で値が空なら、
それは 0 ではない。**

`u64` の生値は**十進文字列**で出る (JavaScript 系で 2^53 超が丸まるのを避けるため)。

## 期間集計とホスト比較

```bash
resarch summarize sa07 sa08 --format json      # 複数ファイルを連結して集計
resarch compare --host web1=web1/sa07 --host web2=web2/sa07
```

判定には**ルール ID と観測根拠**が付く。閾値・継続時間・必要指標・欠損時の扱い・
ルール版が出力に残るので、後から「なぜそう判定したか」を復元できる。

`compare` の共通時間窓は各ホストの観測範囲の**交差**になる。
片方に観測が無い区間を 0 と見なさない。

## sar / sadf 互換出力

既存のスクリプトやツールに食わせるなら互換出力を使う。**本家の期待出力と全文一致**まで
検証してある (21 ケース)。

```bash
resarch -u -f sa07                       # サブコマンドを省略すると sar として振る舞う
resarch -r -f sa07
resarch -n DEV,EDEV -f sa07
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa07
resarch -u -i 600 -f sa07                # 10 分刻みに間引く
resarch -I --int=0,LOC -f sa07           # 割り込みを番号か名前で選ぶ
resarch -A -f sa07 1 1                   # positional interval / count
resarch sar -A -f sa07                   # 明示的な互換入口
resarch sadf -j sa07                     # JSON
resarch sadf -d sa07 -- -d               # DB 形式 (`;` 区切り) でディスク統計
resarch sadf -r sa07 -- -b               # 生カウンタ
```

**`sar` の癖は意図的に再現している。**

- `-I` は数値を取らない
- `-P ALL` と `-P all` は別物
- **`-h` は help ではなく `--pretty --human`**
- `-s` に一致した**最初のレコードは表示されず、前サンプル (基準値) として消費される**

**互換出力では欠落がゼロ補完される。** 本家が「その世代に無いフィールドを 0 埋めした
構造体」を読むため。欠落を欠落として知りたいなら独自出力を使うこと。

### デバイス名について

`sar -d` のデバイス名列は `dev8-0` の形で出る。本家は `major:minor` を**実行ホストの**
`/dev` / `/sys` で名前に解決するが、それは他ホストで採取したログには誤った名前を与える。
`--dev=` のマッチングも `dev<major>-<minor>` に対して行う。

## 旧世代のファイルを変換する

```bash
resarch sadf -c sa07 > sa07-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa07 -O hz=250 > out       # 仮定する HZ を上書き
```

変換後のバイナリは **stdout のみ**、進捗と警告は stderr。

旧世代のヘッダは HZ を持たないため、直読・変換とも既定は USER_HZ=100。
`CONFIG_HZ` とは別の値であり、壁時計の差から推定しない。
生成元の tick 周波数が分かる場合だけ `-O hz=` で上書きする。採用値と出所は stderr に出る。

## 落とし穴

独自コマンドの `--from` / `--to` の `hh:mm[:ss]` は **`--timezone` の基準**で解釈する
(既定は実行環境のローカル。時刻表示も同じ基準)。`--timezone local|utc|<IANA 名>`、
`--utc` はその別名。10 桁の epoch 秒はタイムゾーンの影響を受けず、機械可読形式の
`start_epoch` / `end_epoch` も epoch 秒のまま。

| 症状 | 原因と対処 |
|---|---|
| 時刻が UTC で出ない / 以前の出力と食い違う | 独自コマンドの表示既定が実行環境のローカルタイムゾーンになった。`--utc` (= `--timezone utc`) で従来の `...Z` 表記に戻る。`info` / `identify` と互換入口 (`sar` / `sadf` / `sa2sar`) は対象外で、`--timezone` を持たない。`detect --format json` / `ndjson` の `report_timezone` が、実際に使った基準を示す |
| `--from` / `--to` の効き方がコマンドで違う | `show` は**表示行**、`summarize` / `compare` は**集計期間そのもの**、`detect` は**報告範囲だけ** (比較基準の材料は絞らない)。`detect` だけ違うのは、狭い調査範囲の外から比較材料を取る必要があるため |
| `summarize --from` で結果が空になる | `--from` に一致した最初のレコードは**前サンプルとして消費される**ので、範囲内のレコードが 1 本だけでは区間が作れない。stderr に理由が出る |
| `hh:mm:ss` 指定が初日で打ち切られない | 仕様どおり。時刻指定は**毎日の時刻**として比較する (`sar -s` / `-e` と同じ) |
| `sar -A` で期待した activity が出ない | **magic が現行版と違う activity は本家も表示しない**。`resarch info <file>` で magic を確認する。独自出力 (`show`) なら出る |
| 割り込みの CPU 別内訳を見る | `show --activity irq --irq-cpus`。`cpu` 次元に `all` と CPU 番号を出す。旧ファイルで内訳が記録されていなければ `all` だけ |
| SVG グラフを出す | `sadf -g sa07 -- -u -P ALL > cpu.svg`。共通計算値の独自描画。`-O autoscale,packed,customcol` は未対応として拒否。PCP (`-l`) も未対応 |
| `sadf` で `--dev=` などが効かない | item 名フィルタは `sar` 側にしか効かない (既知の未実装) |
| 破損したファイルで止まる | `--lenient` で診断つきに読み飛ばす。既定 (`--strict`) は疑わしいデータをエラーにする |
| 大きなファイルで遅い | `--activity` で絞る (選択外の activity はデコードせず読み飛ばす。全件比 2.8 倍速)。`--jobs` で並列度を指定 |

## 出力先の規約

- **データは stdout、診断は stderr。** 互換出力に独自の警告を混ぜない
- 読めないファイルがあれば**非ゼロ終了**する (部分結果でも)
- `-f` はディレクトリ指定にも対応し、`SA_DIR` (既定 `/var/log/sa`) 配下の
  `saDD` / `saYYYYMMDD` を **mtime 比較**で選ぶ

## 調査の流れ (例)

```bash
# 1. どんなファイルか (読めない世代でも答える)
resarch identify /var/log/sa/sa07
resarch info /var/log/sa/sa07

# 2. 当たりを付ける。ここで時刻と指標が分かる
resarch detect /var/log/sa/sa07

# 3. detect が指した時刻の周辺を本家書式で見る
resarch -u -P ALL -s 16:00:00 -e 17:00:00 -f /var/log/sa/sa07

# 4. 数値を自分で扱う
resarch show /var/log/sa/sa07 --activity cpu,disk,memory \
  --from 16:00 --to 17:00 --format ndjson --values both

# 5. 前日と比べる
resarch compare --host d06=/var/log/sa/sa06 --host d07=/var/log/sa/sa07
```

**2 を飛ばさないこと。** `detect` は「評価できなかった系列」まで報告するので、
「見ていない範囲」を把握したうえで次を絞れる。
