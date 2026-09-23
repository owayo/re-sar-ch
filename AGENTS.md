# reSARch — 開発時の指針

sysstat の `sa` バイナリを単体で解析する CLI。バイナリ名は `resarch`、crate 名は `re-sar-ch`。

設計の根拠は [`docs/design.md`](docs/design.md)、フォーマット仕様は [`docs/format/`](docs/format/)
にある。**実装を変える前にそちらを読むこと。**

## 層の責務

```
format  … 世代 (format_magic) と生成元 ABI の差を吸収し、レコード境界を確定する
layout  … 43 activity のフィールド定義と列メタデータ
model   … 統一ドメインモデル (Availability / Counter / ActivityId)、環境からの表示方針
series  … 差分・レート・不連続の判定、レコード対の走査
analyze … 期間集計とルール ID 付きの判定
detect  … 異変の当たり付け (観測の提示。断定や確率は出さない)
output  … 書式化のみ
convert … 旧世代 → 現行世代の書き直し (sadf -c 相当)。唯一の書き手
cli     … 引数解析 (sar / sadf 互換パーサは手書き、独自サブコマンドは clap)
skill   … AI エージェント向けスキルの埋め込みとインストール (skill-install)
```

守るべき境界:

- **出力層で値を計算しない。** 同じ指標を `sar` 互換テキストと JSON で出したときに
  形式ごとに計算がずれる事故を、層の分離で防いでいる。
- **互換出力 6 形式 (`sar` テキスト / `sadf -d -p -r -j -x`) は同じ値を出す。**
  片方だけ直して終わりにしない。規則の一覧は
  [`docs/format/03-output-format.md`](docs/format/03-output-format.md) §1.11。
  実際に「1 形式だけ規則が抜けている」事故が 5 件起きている。
- **欠落とゼロを混同しない。** その世代のファイルに無いフィールドは
  `Availability::UnsupportedBySource`。`0` にすると平均や閾値判定が静かに誤る。
  ただし**互換出力ではゼロ補完する** (本家がゼロ埋めした構造体を読むため)。
  `MissingInSample` / 不連続 / 未実装とは分ける。
- **ストライドは常に `file_activity.size` の申告値。** 構造体定義から計算した値を使わない。
- **`detect` は確率を出さない。** 優先度 (順序尺度) と根拠の充足度を別のフィールドに持つ。
  「評価できなかった」を「検出なし」にしない。規律の一覧は `docs/design.md` §11.2。
- **時刻の基準は 1 箇所で決める。** 独自出力の表示・`--from` / `--to` の比較・
  `detect` の報告範囲の日内境界は、すべて `model::timezone::DisplayTz` から導く
  (既定は実行環境のローカル)。片方だけ動かすと、画面の 09:00 と `--from 09:00` が
  別の時刻を指す。日内秒を `epoch % 86_400` で出さない (UTC 固定になる)。
  互換出力 (`sar` / `sadf` / `sa2sar`) はここを使わず本家の規則に従う。
  根拠は `docs/design.md` §9.0.1。
- **環境変数を読むのは CLI 層だけ。** 本家 `sar` が書式に使う環境変数
  (`S_TIME_FORMAT` / `S_REPEAT_HEADER`) の判定は `model::sysstat_env` の純粋関数に置き、
  実際の読み取りは `main` の 1 箇所で済ませて、出力層へは**解決済みの値**を
  `SarTextOptions` に載せて渡す。出力層が `std::env::var` を直接呼ぶと、
  同じ入力・同じオプションでも出力が変わる隠れた依存になり、ライブラリとして
  呼んだときやテストの並行実行で再現しない (`set_var` は edition 2024 で `unsafe`)。
- **別の版の `sar` を再現する経路は現行版と共有しない。**
  `--sar-profile sysstat-10.1.5-el7` (RHEL / CentOS 7 の `sar`) の計算は `series::el7`、
  描画は `output::sar_el7`、引数は `cli::sar_el7_args` にあり、
  `series::compute` / `output::sar_text` / `cli::sar_args` を分岐させない。
  式・C の型の幅・演算順序・平均の丸め・描画の流れが版ごとに違い、共有すると片方の修正が
  もう片方の出力を黙って変える。「出力層で値を計算しない」は el7 経路でも同じ。
  プロファイルはファイルの版から自動で選ばない (`docs/design.md` §9.1)。
- **数学的に等価な式へ整理しない。** 本家が `x / 10.0` と書くところを `x * 0.1` にすると、
  0.1 が二進で正確に表せないため丸め境界で 1 桁ずれる (`%util` で実際に起きた)。
  本家の演算の順序と割り算・掛け算の別をそのまま写す。

## 利用者から見える変更をしたとき

`CHANGELOG.md` の「未リリース」に足す。**バージョンは CalVer で、番号からは
破壊的変更かどうかが読み取れない。** 互換性を壊す変更は各版の先頭に
「⚠️ 破壊的変更」として置き、戻し方 (どのオプションを付ければ従来どおりか) を書く。

内部のリファクタリングだけなら書かない。書くのは**出力・オプション・キー操作**が
変わったときである。

## 変更したときに必ず回すもの

```bash
cargo test        # 自己整合性テストを含む
make check        # clippy -D warnings + fmt
```

### レイアウト定義 (`src/layout/activities/*.rs`) を触ったとき

`layout::registry::tests::all_definitions_are_self_consistent` が、
レイアウト記述から導出したサイズ・型別個数が宣言値 (`size_lp64` / `types_nr`) と
一致するかを機械的に検査する。**このテストは LP64 しか見ない。**
32bit 特有のずれ (`double` のアラインメント等) は別途テストを足すこと。

`size_lp64` と `types_nr` の値は `docs/format/02-activities.md` の実測表から写す。
推測で書かない。

### `format` 層を触ったとき

本家のテストデータで全世代を走査し、**ファイル末尾まで余りなく読めること**を確認する。

```bash
make fixtures                                    # 本家データを target/fixtures/ へ
cargo test --test conformance -- --include-ignored
cargo run --example scan -- <sa ファイル>        # 単体確認 (exact=true になるか)
```

`exact=true` はレイアウト解釈が正しいことの強い証拠になる。ずれていれば必ず残余バイトが出る。

### el7 の経路 (`series::el7` / `output::sar_el7` / `cli::sar_el7_args`) を触ったとき

```bash
cargo test --test sar_el7
```

これは自作 fixture で el7 の癖を固定するだけなので、値の変わる修正では
**本家 el7 の `sar` とも突き合わせる。** CentOS の vault にある
`sysstat-10.1.5-*.el7*.src.rpm` の全パッチを当ててビルドした `sar` と、
同じ sa ファイル・引数・`LC_ALL=C`・`TZ` で stdout と終了コードを比べる
(手順は `docs/format/05-sysstat-10.1.5-el7.md` §6.2)。
GPL のソースとその出力はリポジトリの外に置く。

## テスト資産の扱い

**本家 sysstat は GPL。そのテストデータと期待出力をこのリポジトリに入れてはいけない。**
`make fixtures` が固定タグから SHA-256 検証つきで `target/fixtures/` へ取得する。

リポジトリに同梱する fixture (`tests/fixtures/`) は
`docs/format/01-file-format.md` のオフセット表から**独立に書き起こしたもの**。
本体の `layouts` / `selfdesc` を参照して生成してはいけない
(同じ誤りが往復して検出できなくなる)。

## 実データを扱うときの注意

運用中のホストから採取した `sa` ファイルは `nodename` にホスト名が埋め込まれている。
**実データとその出力例を、リポジトリ・コミットメッセージ・ドキュメントへ持ち込まないこと。**
公開物に載せる出力例は自作 fixture から生成する。

## 仕様で踏みやすい落とし穴

| 項目 | 正しい理解 |
|---|---|
| `unsigned long` の幅 | 自己記述形式の統計領域では **8 バイトのスロット**。32bit 生成では先頭 4 バイトだけが値。旧形式は世代別の実測表に従う。特に 2.2 の列選択形式は padding なしの 4 / 8 バイト |
| `MAP_SIZE` の判定 | `MAP_SIZE(types_nr) <= 申告サイズ`。等号ではない |
| `0x2175` の判別 | `header_size` が同じ 328 でも中身が違う変種がある。型別個数を見る |
| `0x2173` の RESTART | 後続の volatile activity リストで以降の item 数が変わる |
| 拡張レコード (5〜15) | `0x2175` では統計を伴わない。旧世代には存在しないので統計扱いが正しい |
| item 数の上限 | activity 別の `nr_max` が必要。汎用上限だけでは細工ファイルが通る |
| `nr × nr2 × size` | u32 を超え得る。必ず checked 演算で計算する |

## 性能について

解析速度は要件。最適化を入れるときは**必ずベンチ差分を添える**。

```bash
cargo bench                # benches/decode.rs (criterion)
```

設計上の前提:

- 選択されていない activity は `nr × nr2 × size` を加算するだけでデコードしない
- 時刻フィルタ範囲外のレコードは `record_header` だけ読んで本体をスキップする
- レイアウト解釈は `DecodePlan` として事前解決し、ホットパスから追い出す
- 出力はストリーミング。`stdout` は `BufWriter` で包み、ロックは 1 回だけ取る
