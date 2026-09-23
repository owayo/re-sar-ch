# CentOS 3・4・5 のモノリシック sa 形式

CentOS Vault の sysstat 5.0.5-11.rhel3、5.0.5-27.el4、7.0.2-13.el5 の SRPM に全パッチを適用し、構造体のサイズとオフセットを C の `sizeof` / `offsetof` で確認した。ソースと測定プログラムはリポジトリの外に置く。

- [CentOS 3.9 SRPM](https://vault.centos.org/3.9/os/SRPMS/sysstat-5.0.5-11.rhel3.src.rpm)
- [CentOS 4.9 SRPM](https://vault.centos.org/4.9/updates/SRPMS/sysstat-5.0.5-27.el4.src.rpm)
- [CentOS 5.11 SRPM](https://vault.centos.org/5.11/os/SRPMS/sysstat-7.0.2-13.el5.src.rpm)

ほかの旧形式を含む全22形式の対応と制限は [旧形式一覧](09-legacy-generations.md) を参照。

## 構造

240 バイトのヘッダに続き、各統計レコードは固定部、CPU 別配列、個別 IRQ 配列（flag 0x10 のとき 256 × 4）、PID 配列、シリアル配列、CPU 別 IRQ 配列、ネットワーク配列、ディスク配列（0x2163 はヘッダの件数分、0x2169 は flag 0x800 のとき）の順に並ぶ。再起動レコード（種別 2）は固定部だけ。種別 1 は統計、種別 3 は `0x2169` の最終統計 (`0x2163` にはない)。activity のビットは CPU=4、PCSW=3、IRQ=8、SWAP=0x20、IO=0x40、MEMORY=0x20080、SERIAL=0x100、NET_DEV=0x200、NET_EDEV=0x400、DISK=0x800、PAGE=0x10000、KTABLES=0x40000、NET_SOCK=0x80000、QUEUE=0x100000。0x2169 には NFS=0x4000、NFSD=0x8000 もある。

0x2163 の `sa_proc` は実 CPU 数 − 1（0 は単一 CPU）、CPU 別配列は非ゼロのときだけ実 CPU 数分ある。0x2169 は実 CPU 数そのもの（0 は非 SMP の単一 CPU）。`uptime0` は CPU 別配列がないとき未設定なので `uptime` を使う。

現行配置へのバイナリ変換はせず、固定部の各統計を重なりのあるビューとして読む。CPU 集約行と CPU 別行は異なる読み取り計画を持つ。存在しないフィールドは `UnsupportedBySource` のまま保持する。互換書式は現行 sysstat の列構成を使い、当時の sar の列構成・表示を完全再現するプロファイルではない。

## 対応範囲と表示

CPU 別 IRQ 配列は境界の検証に使用し、デコードは IRQ 総数と 256 個の個別 IRQ 総数まで。
PID 付きパイプストリームは明示的に拒否する。旧カーネルの super / dquot / rtsig など、現在のドメインモデルにない値は公開しない。
シリアルの回線番号は 0 起点で `u32::MAX` が未使用、ネットワーク名 `?` も未使用枠なので出力しない。
CPU の `all` は現在の共通計算規則で CPU 別値を集約するため、固定部にある古い集約値と採取時点のずれがあれば旧版の表示との差が出る。
ファイルヘッダ・レコード境界の解析は format 層、フィールド対応と未使用枠の規則は layout 層、差分や率は series 層に置く。
現行書式のバイナリへの変換 (`convert`) の対象は増やさない。

## ABI

0x2169 はヘッダの 43 バイト目に long 幅を持つ。0x2163 は long 幅とアーキテクチャを記録しないため、既定で 8 バイトと仮定し診断に明記する。ライブラリの `OpenOptions.legacy_long_bytes` で 4 または 8 を指定できる。エンディアンは magic で判定する。以下の対応済み構造体の位置は両方の幅で共通（対象外の PID 構造体は幅で配置も変わる）。long は 8 バイトスロットの先頭 4 / 8 バイトを値として読む。

## 実測オフセット

表の幅は LP64。long のみ ILP32 で 4 に変わる。0x2163 の CentOS 3 / 4 の配置は同じ。採取ファイルのサイズ差はシリアル配列の件数差である。

### 0x2163

| 構造体 | フィールド | オフセット | 型の幅 |
|---|---|---:|---:|
| file_hdr | **全体サイズ** | — | 240 |
| file_hdr | sa_actflag | 0 | 4 |
| file_hdr | sa_magic | 4 | 2 |
| file_hdr | sa_st_size | 6 | 2 |
| file_hdr | sa_nr_pid | 8 | 4 |
| file_hdr | sa_irqcpu | 12 | 4 |
| file_hdr | sa_ust_time | 16 | long (4 / 8) |
| file_hdr | sa_nr_disk | 24 | 4 |
| file_hdr | sa_proc | 28 | 4 |
| file_hdr | sa_serial | 32 | 4 |
| file_hdr | sa_iface | 36 | 4 |
| file_hdr | sa_day | 40 | 1 |
| file_hdr | sa_month | 41 | 1 |
| file_hdr | sa_year | 42 | 1 |
| file_hdr | sa_sysname | 43 | 65 |
| file_hdr | sa_nodename | 108 | 65 |
| file_hdr | sa_release | 173 | 65 |
| file_stats | **全体サイズ** | — | 288 |
| file_stats | record_type | 0 | 1 |
| file_stats | hour | 1 | 1 |
| file_stats | minute | 2 | 1 |
| file_stats | second | 3 | 1 |
| file_stats | nr_processes | 4 | 4 |
| file_stats | ust_time | 8 | long (4 / 8) |
| file_stats | uptime | 16 | long (4 / 8) |
| file_stats | uptime0 | 24 | long (4 / 8) |
| file_stats | processes | 32 | long (4 / 8) |
| file_stats | context_swtch | 40 | 4 |
| file_stats | cpu_user | 44 | 4 |
| file_stats | cpu_nice | 48 | 4 |
| file_stats | cpu_system | 52 | 4 |
| file_stats | cpu_idle | 56 | long (4 / 8) |
| file_stats | cpu_iowait | 64 | long (4 / 8) |
| file_stats | irq_sum | 72 | long (4 / 8) |
| file_stats | pgpgin | 80 | long (4 / 8) |
| file_stats | pgpgout | 88 | long (4 / 8) |
| file_stats | pswpin | 96 | long (4 / 8) |
| file_stats | pswpout | 104 | long (4 / 8) |
| file_stats | dk_drive | 112 | 4 |
| file_stats | dk_drive_rio | 116 | 4 |
| file_stats | dk_drive_wio | 120 | 4 |
| file_stats | dk_drive_rblk | 124 | 4 |
| file_stats | dk_drive_wblk | 128 | 4 |
| file_stats | frmkb | 136 | long (4 / 8) |
| file_stats | bufkb | 144 | long (4 / 8) |
| file_stats | camkb | 152 | long (4 / 8) |
| file_stats | tlmkb | 160 | long (4 / 8) |
| file_stats | frskb | 168 | long (4 / 8) |
| file_stats | tlskb | 176 | long (4 / 8) |
| file_stats | caskb | 184 | long (4 / 8) |
| file_stats | file_used | 192 | 4 |
| file_stats | inode_used | 196 | 4 |
| file_stats | super_used | 200 | 4 |
| file_stats | super_max | 204 | 4 |
| file_stats | dquot_used | 208 | 4 |
| file_stats | dquot_max | 212 | 4 |
| file_stats | rtsig_queued | 216 | 4 |
| file_stats | rtsig_max | 220 | 4 |
| file_stats | sock_inuse | 224 | 4 |
| file_stats | tcp_inuse | 228 | 4 |
| file_stats | udp_inuse | 232 | 4 |
| file_stats | raw_inuse | 236 | 4 |
| file_stats | frag_inuse | 240 | 4 |
| file_stats | pgfault | 248 | long (4 / 8) |
| file_stats | pgmajfault | 256 | long (4 / 8) |
| file_stats | dentry_stat | 264 | 4 |
| file_stats | load_avg_1 | 268 | 4 |
| file_stats | load_avg_5 | 272 | 4 |
| file_stats | load_avg_15 | 276 | 4 |
| file_stats | nr_running | 280 | 4 |
| file_stats | nr_threads | 284 | 4 |
| stats_one_cpu | **全体サイズ** | — | 32 |
| stats_one_cpu | per_cpu_idle | 0 | long (4 / 8) |
| stats_one_cpu | per_cpu_iowait | 8 | long (4 / 8) |
| stats_one_cpu | per_cpu_user | 16 | 4 |
| stats_one_cpu | per_cpu_nice | 20 | 4 |
| stats_one_cpu | per_cpu_system | 24 | 4 |
| stats_one_cpu | pad | 28 | 4 |
| stats_serial | **全体サイズ** | — | 16 |
| stats_serial | rx | 0 | 4 |
| stats_serial | tx | 4 | 4 |
| stats_serial | line | 8 | 4 |
| stats_serial | pad | 12 | 4 |
| stats_net_dev | **全体サイズ** | — | 144 |
| stats_net_dev | rx_packets | 0 | long (4 / 8) |
| stats_net_dev | tx_packets | 8 | long (4 / 8) |
| stats_net_dev | rx_bytes | 16 | long (4 / 8) |
| stats_net_dev | tx_bytes | 24 | long (4 / 8) |
| stats_net_dev | rx_compressed | 32 | long (4 / 8) |
| stats_net_dev | tx_compressed | 40 | long (4 / 8) |
| stats_net_dev | multicast | 48 | long (4 / 8) |
| stats_net_dev | collisions | 56 | long (4 / 8) |
| stats_net_dev | rx_errors | 64 | long (4 / 8) |
| stats_net_dev | tx_errors | 72 | long (4 / 8) |
| stats_net_dev | rx_dropped | 80 | long (4 / 8) |
| stats_net_dev | tx_dropped | 88 | long (4 / 8) |
| stats_net_dev | rx_fifo_errors | 96 | long (4 / 8) |
| stats_net_dev | tx_fifo_errors | 104 | long (4 / 8) |
| stats_net_dev | rx_frame_errors | 112 | long (4 / 8) |
| stats_net_dev | tx_carrier_errors | 120 | long (4 / 8) |
| stats_net_dev | interface | 128 | 16 |
| stats_irq_cpu | **全体サイズ** | — | 8 |
| stats_irq_cpu | interrupt | 0 | 4 |
| stats_irq_cpu | irq | 4 | 4 |
| disk_stats | **全体サイズ** | — | 24 |
| disk_stats | major | 0 | 4 |
| disk_stats | index | 4 | 4 |
| disk_stats | nr_ios | 8 | 4 |
| disk_stats | rd_sect | 12 | 4 |
| disk_stats | wr_sect | 16 | 4 |
| disk_stats | pad | 20 | 4 |


### 0x2169

| 構造体 | フィールド | オフセット | 型の幅 |
|---|---|---:|---:|
| file_hdr | **全体サイズ** | — | 240 |
| file_hdr | sa_ust_time | 0 | long (4 / 8) |
| file_hdr | sa_actflag | 8 | 4 |
| file_hdr | sa_nr_pid | 12 | 4 |
| file_hdr | sa_irqcpu | 16 | 4 |
| file_hdr | sa_nr_disk | 20 | 4 |
| file_hdr | sa_proc | 24 | 4 |
| file_hdr | sa_serial | 28 | 4 |
| file_hdr | sa_iface | 32 | 4 |
| file_hdr | sa_magic | 36 | 2 |
| file_hdr | sa_st_size | 38 | 2 |
| file_hdr | sa_day | 40 | 1 |
| file_hdr | sa_month | 41 | 1 |
| file_hdr | sa_year | 42 | 1 |
| file_hdr | sa_sizeof_long | 43 | 1 |
| file_hdr | sa_sysname | 44 | 65 |
| file_hdr | sa_nodename | 109 | 65 |
| file_hdr | sa_release | 174 | 65 |
| file_stats | **全体サイズ** | — | 464 |
| file_stats | uptime | 0 | 8 |
| file_stats | uptime0 | 16 | 8 |
| file_stats | context_swtch | 32 | 8 |
| file_stats | cpu_user | 48 | 8 |
| file_stats | cpu_nice | 64 | 8 |
| file_stats | cpu_system | 80 | 8 |
| file_stats | cpu_idle | 96 | 8 |
| file_stats | cpu_iowait | 112 | 8 |
| file_stats | cpu_steal | 128 | 8 |
| file_stats | irq_sum | 144 | 8 |
| file_stats | ust_time | 160 | long (4 / 8) |
| file_stats | processes | 168 | long (4 / 8) |
| file_stats | pgpgin | 176 | long (4 / 8) |
| file_stats | pgpgout | 184 | long (4 / 8) |
| file_stats | pswpin | 192 | long (4 / 8) |
| file_stats | pswpout | 200 | long (4 / 8) |
| file_stats | frmkb | 208 | long (4 / 8) |
| file_stats | bufkb | 216 | long (4 / 8) |
| file_stats | camkb | 224 | long (4 / 8) |
| file_stats | tlmkb | 232 | long (4 / 8) |
| file_stats | frskb | 240 | long (4 / 8) |
| file_stats | tlskb | 248 | long (4 / 8) |
| file_stats | caskb | 256 | long (4 / 8) |
| file_stats | nr_running | 264 | long (4 / 8) |
| file_stats | pgfault | 272 | long (4 / 8) |
| file_stats | pgmajfault | 280 | long (4 / 8) |
| file_stats | dk_drive | 288 | 4 |
| file_stats | dk_drive_rio | 292 | 4 |
| file_stats | dk_drive_wio | 296 | 4 |
| file_stats | dk_drive_rblk | 300 | 4 |
| file_stats | dk_drive_wblk | 304 | 4 |
| file_stats | file_used | 308 | 4 |
| file_stats | inode_used | 312 | 4 |
| file_stats | super_used | 316 | 4 |
| file_stats | super_max | 320 | 4 |
| file_stats | dquot_used | 324 | 4 |
| file_stats | dquot_max | 328 | 4 |
| file_stats | rtsig_queued | 332 | 4 |
| file_stats | rtsig_max | 336 | 4 |
| file_stats | sock_inuse | 340 | 4 |
| file_stats | tcp_inuse | 344 | 4 |
| file_stats | udp_inuse | 348 | 4 |
| file_stats | raw_inuse | 352 | 4 |
| file_stats | frag_inuse | 356 | 4 |
| file_stats | dentry_stat | 360 | 4 |
| file_stats | load_avg_1 | 364 | 4 |
| file_stats | load_avg_5 | 368 | 4 |
| file_stats | load_avg_15 | 372 | 4 |
| file_stats | nr_threads | 376 | 4 |
| file_stats | nfs_rpccnt | 380 | 4 |
| file_stats | nfs_rpcretrans | 384 | 4 |
| file_stats | nfs_readcnt | 388 | 4 |
| file_stats | nfs_writecnt | 392 | 4 |
| file_stats | nfs_accesscnt | 396 | 4 |
| file_stats | nfs_getattcnt | 400 | 4 |
| file_stats | nfsd_rpccnt | 404 | 4 |
| file_stats | nfsd_rpcbad | 408 | 4 |
| file_stats | nfsd_netcnt | 412 | 4 |
| file_stats | nfsd_netudpcnt | 416 | 4 |
| file_stats | nfsd_nettcpcnt | 420 | 4 |
| file_stats | nfsd_rchits | 424 | 4 |
| file_stats | nfsd_rcmisses | 428 | 4 |
| file_stats | nfsd_readcnt | 432 | 4 |
| file_stats | nfsd_writecnt | 436 | 4 |
| file_stats | nfsd_accesscnt | 440 | 4 |
| file_stats | nfsd_getattcnt | 444 | 4 |
| file_stats | record_type | 448 | 1 |
| file_stats | hour | 449 | 1 |
| file_stats | minute | 450 | 1 |
| file_stats | second | 451 | 1 |
| stats_one_cpu | **全体サイズ** | — | 112 |
| stats_one_cpu | per_cpu_idle | 0 | 8 |
| stats_one_cpu | per_cpu_iowait | 16 | 8 |
| stats_one_cpu | per_cpu_user | 32 | 8 |
| stats_one_cpu | per_cpu_nice | 48 | 8 |
| stats_one_cpu | per_cpu_system | 64 | 8 |
| stats_one_cpu | per_cpu_steal | 80 | 8 |
| stats_one_cpu | pad | 96 | 8 |
| stats_serial | **全体サイズ** | — | 32 |
| stats_serial | rx | 0 | 4 |
| stats_serial | tx | 4 | 4 |
| stats_serial | frame | 8 | 4 |
| stats_serial | parity | 12 | 4 |
| stats_serial | brk | 16 | 4 |
| stats_serial | overrun | 20 | 4 |
| stats_serial | line | 24 | 4 |
| stats_serial | pad | 28 | 4 |
| stats_net_dev | **全体サイズ** | — | 144 |
| stats_net_dev | rx_packets | 0 | long (4 / 8) |
| stats_net_dev | tx_packets | 8 | long (4 / 8) |
| stats_net_dev | rx_bytes | 16 | long (4 / 8) |
| stats_net_dev | tx_bytes | 24 | long (4 / 8) |
| stats_net_dev | rx_compressed | 32 | long (4 / 8) |
| stats_net_dev | tx_compressed | 40 | long (4 / 8) |
| stats_net_dev | multicast | 48 | long (4 / 8) |
| stats_net_dev | collisions | 56 | long (4 / 8) |
| stats_net_dev | rx_errors | 64 | long (4 / 8) |
| stats_net_dev | tx_errors | 72 | long (4 / 8) |
| stats_net_dev | rx_dropped | 80 | long (4 / 8) |
| stats_net_dev | tx_dropped | 88 | long (4 / 8) |
| stats_net_dev | rx_fifo_errors | 96 | long (4 / 8) |
| stats_net_dev | tx_fifo_errors | 104 | long (4 / 8) |
| stats_net_dev | rx_frame_errors | 112 | long (4 / 8) |
| stats_net_dev | tx_carrier_errors | 120 | long (4 / 8) |
| stats_net_dev | interface | 128 | 16 |
| stats_irq_cpu | **全体サイズ** | — | 8 |
| stats_irq_cpu | interrupt | 0 | 4 |
| stats_irq_cpu | irq | 4 | 4 |
| disk_stats | **全体サイズ** | — | 80 |
| disk_stats | rd_sect | 0 | 8 |
| disk_stats | wr_sect | 16 | 8 |
| disk_stats | rd_ticks | 32 | long (4 / 8) |
| disk_stats | wr_ticks | 40 | long (4 / 8) |
| disk_stats | tot_ticks | 48 | long (4 / 8) |
| disk_stats | rq_ticks | 56 | long (4 / 8) |
| disk_stats | nr_ios | 64 | long (4 / 8) |
| disk_stats | major | 72 | 4 |
| disk_stats | minor | 76 | 4 |

