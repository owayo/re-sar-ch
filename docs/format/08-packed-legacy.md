# sysstat 2.2 の列選択形式 (`0x015d`)

確認した一次資料は SuSE Linux 6.4 の `sysstat-2.2-17.src.rpm` に入っている
sysstat 2.2 の `sar.h` と `sar.c`。配布物の保存先は
<https://archive.org/download/suse64/SU6400.006.iso/suse%2Fzq2%2Fsysstat.spm>。
ソース RPM の SHA-256 は
`dcce05c498cd5ab0d9b314eb4433b1875227a9f34a82d6e4360784501b502785`。
GPL のソースはリポジトリ外で調査する。

## ヘッダ (280 バイト)

| オフセット | 幅 | 内容 |
|---:|---:|---|
| 0 | 2 | magic `0x015d`、常に little endian |
| 2 / 3 / 4 | 各 1 | 日 / 月 (0 起点) / 年 (1900 起点) |
| 5 / 6 / 7 | 各 1 | 開始時・分・秒 |
| 8 | 2 | activity flags、常に little endian |
| 10 | 32 | CPU 選択 bitmap、各 byte の下位 bit が小さい番号 |
| 42 | 32 | IRQ 選択 bitmap、同上 |
| 74 | 2 | 1 レコードのバイト数、常に little endian |
| 76 | sizeof(long) | 採取間隔 (秒)、生成元の endian |
| 84 / 149 / 214 | 各 65 | sysname / release / nodename |
| 279 | 1 | sizeof(long)、4 または 8 |

ヘッダの残りから末尾まで同じ大きさのレコードが続く。再起動・コメントレコードはない。
数値列は生成元の endian で、詰めて書かれる。後の構造体形式と異なり、32 bit の long は
**4 バイトだけ**を占める。magic の byte 順から数値列の endian は判定できない。

## レコードの順序

flags が立っている区画だけを以下の順に連結する。各区画・フィールド間に padding はない。

| Flag | フィールド順 | 各フィールドの幅 |
|---:|---|---|
| `0x001` | processes | long |
| `0x002` | context_swtch | 4 |
| `0x004` | cpu_user, cpu_nice, cpu_system, cpu_idle | 4, 4, 4, long |
| `0x008` | irq_sum | 4 |
| `0x010` | pgpgin, pgpgout | 4, 4 |
| `0x020` | pswpin, pswpout | 4, 4 |
| `0x040` | dk_drive, dk_drive_rio, dk_drive_wio, dk_drive_rblk, dk_drive_wblk | 各 4 |
| `0x080` | bitmap で選ばれた CPU を昇順に、user, nice, system, idle | 4, 4, 4, long |
| `0x100` | bitmap で選ばれた IRQ を昇順に | 各 4 |
| `0x200` | frmpg, shmpg, bufpg, campg | 各 long |

本家 2.2 のレコード出力範囲は CPU 0〜31、IRQ 0〜223。bitmap の保存処理は
上限の次の格納ワードも含むため、全選択では範囲外の bit も立つ。
CPU bitmap の先頭 4 バイト、IRQ bitmap の先頭 28 バイトだけをレコード長の
計算に使い、残りの bit は無視する。未知 flag、申告されたレコード長と計算した
長さの相違、ゼロ採取間隔は受け付けない。

## 時刻・統計値の意味と制約

- レコードは時刻・uptime を持たない。開始時刻と採取間隔から時刻を復元する。
  タイムゾーンは保存されていないため UTC と仮定し、診断に明示する。uptime は
  実際の起動後時間ではなく先頭レコードからの相対時間を使う。本家も読み直し時に
  同じ間隔を積算している。
- 全 CPU 行の累積値は本家が CPU 数で割ってから保存する。記録値をそのまま保持する。
- CPU / IRQ の選択 bitmap は疎になり得る。番号を詰めた連番へ変えないため、現在は
  各配列の境界だけ検証する。全 CPU 行と IRQ 合計は読み取る。
- メモリは kB ではなくページ数。ページサイズが保存されていないので kB として
  読み替えず、現在のメモリ指標は `UnsupportedBySource` とする。
- paging と I/O 転送量は生成カーネルによって単位が異なる。2.2 のソースコメントは
  ページ数 / 1024 バイト単位と記しているが、後の本家資料ではカーネル 2.2.x と
  2.4 以降の差が明記されており、そのコメントだけでは単位を確定できない。
  現在は kB / 512 バイト単位へ誤対応させないため `UnsupportedBySource` とする。
  I/O 回数は読み取る。
- source ABI の endian は別途仮定が必要であり、診断に明示する。
  ライブラリでは `OpenOptions.legacy_endian` で指定できる。未指定時は little endian。
- `exact=true` はレコード境界が末尾まで一致することを表す。上の未解釈項目や
  本家と描画・計算が一致するかどうかとは別の検証である。
