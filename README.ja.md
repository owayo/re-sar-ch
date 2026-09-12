<h1 align="center">reSARch</h1>

<p align="center">
  sysstat のバイナリログ (<code>sa</code> ファイル) を単体で解析する CLI。<br>
  <code>sar</code> も <code>sadf</code> も C ライブラリも要りません。
</p>

<p align="center">
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/owayo/re-sar-ch/actions/workflows/release.yml"><img src="https://github.com/owayo/re-sar-ch/actions/workflows/release.yml/badge.svg?branch=main" alt="Release"></a>
  <a href="https://github.com/owayo/re-sar-ch/releases"><img src="https://img.shields.io/github/v/release/owayo/re-sar-ch" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.ja.md">日本語</a>
</p>

---

## 何のために作ったか

`sar` のログ (`/var/log/sa/saXX`) は C 構造体をそのままディスクへ書いた形式です。
書くのは速いが読むのが面倒で、通常は**同じ世代・同じアーキテクチャの sysstat** が必要になります。
5 年前に 32bit の PowerPC 機で採取したログを、手元のマシンの `sar` で開くことはできません。

reSARch はバイト列を直接読みます。sysstat が出荷した全フォーマット世代を知っており、
構造体の配置を「実行ホスト」ではなく「**ファイルを書いたホストの ABI**」から解決するため、
Rust が動く環境ならどこでも動きます。macOS や Windows で Linux のログを読むこともできます。

## 何を読めるか

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

## インストール

[Releases](https://github.com/owayo/re-sar-ch/releases) からバイナリを取得するか、ビルドします。

```bash
cargo install --git https://github.com/owayo/re-sar-ch
```

```bash
# macOS (Apple Silicon)
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-aarch64-apple-darwin.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

## 使い方

### `sar` と同じ引数で使う

reSARch は `sar` の引数構文をそのまま受け付けます。サブコマンドを省略すると `sar` として振る舞います。

```bash
resarch -u -f sa01                       # CPU 使用率
resarch -r -f sa01                       # メモリ
resarch -n DEV,EDEV -f sa01              # ネットワークインターフェース
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa01
resarch sar -A -f sa01                   # 明示的な互換入口
resarch sadf -j sa01                     # sadf 互換 JSON
```

`sar` の癖も意図的に再現しています。`-I` は数値を取らない、`-P ALL` と `-P all` は別物、
`-h` は help ではなく `--pretty --human`、`-s` に一致した最初のレコードは
表示されず基準値として消費される、といった挙動です。

### 独自のサブコマンド

```bash
resarch info sa01                        # 世代・ABI・activity 一覧
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # エージェントやパイプラインへ流す用
resarch detect sa01                      # いつ・何に異変があったか
resarch summarize sa01 sa02 --format json
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

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

### 旧世代のファイルを変換する

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # 仮定する HZ を上書きする
```

旧世代のヘッダは HZ を持たず、本家の `sadf -c` は**変換を実行したマシンの HZ** を
書き込むため、同じ入力でも実行環境で出力が変わります。reSARch はファイル自身の
`uptime` カウンタから推定し、**再起動をまたぐ対は使いません**。
採用した値とその出所は必ず報告します。

## 出力の正確さをどう検証しているか

本家 `sysstat` がテストスイートに持っている期待出力を**全件・1 行ずつ**突き合わせています。
対象は 21 ケースです。

| | |
|---|---:|
| 全文一致 | 16 |
| マスクして一致 | 4 |
| 不一致 | 0 |
| 比較不能 | 1 |

マスクしている 4 件が潰しているのは `A_DISK` のデバイス名列だけです。
本家は `major:minor` を**読んだホストの** `/dev` / `/sys` で名前に解決するので、
`sda1` という名前は期待出力を作ったマシンの構成であって、ファイルの中身ではありません。
reSARch は他ホストで採取したログに誤った名前を付けないよう、あえて `dev8-1` のまま出します。

比較できない 1 件は SVG を描く `sadf -g` です。対応する出力形式が無いので、
毎回「比較不能」として集計に出し、黙って合格扱いにはしていません。

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
書き出すのは「読んだファイルを別世代の配置で書き直したもの」(`sadf -c`) だけで、値は変えません。
また「読むファイルの世代」と「再現する出力の世代」は別の設定で、
v10 のファイルを v12 の `sar` 書式で出すこともその逆もできます。

現時点で受け付けるが動作しないオプションは、実行時に理由を添えて拒否します。

| オプション | 扱い |
|---|---|
| `sar -o` / `--sadc` | 採取は対象外なので明示的にエラー |
| `sadf -g` / `-l` | SVG・PCP は未対応 |
| `sar -i` / positional の `interval` `count` | 未対応 (全レコードを出す) |
| `--int=` | 未対応 (`A_IRQ` は行列レイアウトのため) |
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

## 開発

```bash
make build        # デバッグビルド
make test         # 単体テストと統合テスト
make check        # clippy + fmt
make fixtures     # 本家のテストデータを取得 (下記参照)
make release      # 最適化ビルド
```

本家 sysstat は GPL なので、**そのテストデータも期待出力もこのリポジトリには同梱していません**。
`make fixtures` が固定タグから SHA-256 検証つきで `target/fixtures/` へ取得し、
適合テストにのみ使います。リポジトリに含まれる fixture は自分で書き起こしたもので、
本体と同じ MIT ライセンスです。

reSARch は `sysstat` のソースを移植したものではなく、ディスク上のフォーマットから
独立に実装したものです。

## ライセンス

MIT — [LICENSE](LICENSE) を参照してください。
