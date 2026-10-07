# Usage

This page covers each entry point of `resarch` in detail. The [README](../README.md#usage) shows the common cases and lists the commands.

## As a drop-in for `sar`

Omit the subcommand to read saved files with `sar`-compatible arguments. Output defaults to the sysstat 12.8.0 profile, regardless of the input file's generation. See [compatibility](compatibility.md) for supported options and output profiles:

```bash
resarch -u -f sa01                       # CPU utilisation
resarch -r -f sa01                       # memory
resarch -n DEV,EDEV -f sa01              # network interfaces
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa01
resarch -u -i 600 -f sa01                # thin the samples to ~10-minute steps
resarch -I --int=0,LOC -f sa01           # pick interrupts by number or name
resarch sar -A -f sa01                   # explicit compatibility entry point
resarch sadf -j sa01                     # sadf-compatible JSON
resarch sadf -g sa01 -- -u -P ALL > cpu.svg  # SVG charts (reSARch drawing)
resarch --sar-profile sysstat-10.1.5-el7 -u -f sa01  # as RHEL/CentOS 7's own sar prints it
```

The quirks are reproduced deliberately: `-I` takes no number, `-P ALL` differs from `-P all`, `-h` means `--pretty --human` rather than help, and the first record matched by `-s` is consumed as the baseline rather than displayed.

## Saving an sa binary as sar text

```bash
resarch sa2sar sa13 -o sar13             # All activities, averages, restarts and comments
resarch sa2sar sa13 --utc -o sar13-utc   # Use UTC timestamps
resarch sa2sar sa13                     # Write to stdout for piping
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13   # what a RHEL/CentOS 7 host's sa2 writes
```

The default is equivalent to `sar -A -C -t -f sa13`, using timestamps recorded by the source host. Omit `-o` or use `-o -` for stdout. Existing destination files are never overwritten; failed conversion leaves no partial destination file.

`--sar-profile sysstat-10.1.5-el7` renders the text the way the host's own `sa2` does on RHEL / CentOS 7 — the columns, the arithmetic and the rounding of averages all follow sysstat 10.1.5 as Red Hat ships it. Use it to generate a `sarDD` that `sa2` has not written. Reproduction depends on the supported RPM version, page size, and device names. See [Reproducing RHEL / CentOS 7's `sar`](compatibility.md#reproducing-rhel--centos-7s-sar). Old and big-endian inputs are read directly. CI compares CLI-generated files against five upstream golden reports covering three old versions, a current format and big-endian data. Only disk names unavailable on the reading host use the existing comparison mask.

## Its own commands

```bash
resarch tui sa01                         # browse interactively (TUI)
resarch identify sa01 sa02               # which sysstat wrote each file
resarch info sa01                        # generation, ABI, activity table
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # for feeding an agent or a pipeline
resarch detect sa01                      # where and what looks off
resarch detect sa01 --verbose            # every detection's breakdown and interpretations
resarch detect sa01 --lang ja            # report in Japanese (see "Output language")
resarch summarize sa01 sa02 --format json
resarch summarize sa01 sa02 sa03 --from 09:00 --to 18:00  # 09:00-18:00 local time each day
resarch show sa01 --activity irq --irq-cpus --format ndjson  # per-CPU interrupt detail
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

For `summarize` and `compare`, `--from` / `--to` narrow **the aggregation period itself**: samples outside the range enter neither the mean, the p95, nor the delta totals, and the period bounds shrink with them. `detect` reads the same two options differently — they narrow what gets reported, not the material its comparison basis is built from. Native commands interpret `hh:mm[:ss]` in **the same timezone they display**, local by default; 10-digit epoch seconds are also accepted. `compare` JSON contains `comparisons` and `skipped_metrics`, including the hosts missing each skipped metric. IRQ rows carry a `cpu` dimension (`all` or a zero-based CPU number); old files without CPU detail emit only `all`.

### Timestamp timezone

The native subcommands (`show`, `summarize`, `detect`, `compare`, `tui`) display timestamps in **the machine's local timezone**. `--timezone <TZ>` changes the basis and accepts `local` (the default), `utc`, or an IANA name such as `Asia/Tokyo`. `--utc` is an alias for `--timezone utc`; giving both is an error.

```bash
resarch summarize sa01                        # 2026-08-31 15:10:01+09:00
resarch summarize sa01 --utc                  # 2026-08-31 06:10:01Z
resarch summarize sa01 --timezone Asia/Tokyo  # Japan time on any machine
```

The same basis interprets `hh:mm[:ss]` in `--from` / `--to`, so the 09:00 on screen and the `--from 09:00` you type never disagree. 10-digit epoch seconds stay timezone independent. Timezones are named by their IANA name (`Asia/Tokyo`), falling back to a numeric offset (`+09:00`) where no IANA name can be determined. Abbreviations like `JST` are never used: they collide, and on a DST transition day they cannot tell apart the two occurrences of the same wall clock.

Machine-readable formats (json / ndjson / csv) keep `start_epoch` / `end_epoch` as epoch seconds regardless of `--timezone`. Only `detect --format json` / `--format ndjson` carry `report_timezone`, recording which wall clock `--from` / `--to` were read against.

`info` and `identify` have no such option: they print what the file header states, so there is nothing to reopen in the reader's timezone. The compatible entry points (`resarch sar`, `resarch sadf`, `resarch sa2sar`) keep upstream sysstat's own rules and are unaffected by `--timezone` as well.

## Browsing interactively (TUI)

```bash
resarch tui sa01
resarch tui sa01 --activity cpu,disk,memory   # open with a narrowed set
```

Recorded activities become tabs; pick an item and read its time series as a table with a graph above it.

Item (`i`) and column (`c`) pickers stay within the terminal when it shrinks; widen it to read clipped labels. Large finite graph values use scaled coordinates and scientific axis labels in the original units.

The screen looks roughly like this (the TUI's labels are in Japanese; column positions and values are illustrative):

```text
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
←→ activity  ↑↓ 時刻  i item (3)  / 絞り込み  home/end 端  c 列  [] 送り  v グラフ  ? help  esc 終了
```

Letter keys use lower case. The main controls are listed below.

| Key | Action |
|---|---|
| `←` / `→` | switch activity |
| `shift` + `←` / `→` | scroll the table sideways |
| `↑` / `↓`, `pgup` / `pgdn` | move through time |
| `home` / `end` | first / last |
| `i` | pick an item (device, interface, CPU) |
| `/` | filter items by name |
| `c` | choose columns (both the table's and the graph's) |
| `[` / `]` | previous / next metric (only those the table shows; on the MEMORY tab the comparison graph comes first) |
| `v` | show or hide the graph |
| `?` | key reference |
| `esc` | quit from the main screen; close a popup or cancel filter input |
| `ctrl-c` | quit from any screen |

Inside the `c` popup, `space` adds or removes a column from the table, `a` switches between every column and the default, and `enter` applies — the row under the cursor becomes the graphed metric, so applying without moving the cursor leaves the graph as it was. `esc` cancels, leaving both the table and the graph alone.

For activities with many columns, such as memory, the table limits the visible columns to keep values readable.

- **Columns that never carried a value are dropped by default.** Fields the source generation never had, and metrics that are not implemented, show `—` on every row and do nothing but push the columns you wanted off the screen.
- **Columns that do not fit the width are dropped rather than squashed.** `shift` + `←` / `→` scrolls to the rest, and the title shows the position (`9-18/23 列`) so it is clear more follow to the right. Columns held back from the table are counted separately.

`c` opens the column list. Columns without values are listed too, greyed out and annotated as such. Widths are computed from every sample, so scrolling through rows never makes them jump.

The graph plots one series — the selected activity, item and metric (the MEMORY tab opens on a three-series comparison instead; [see below](#the-memory-tabs-comparison-graph)). **The header of the graphed column is drawn in the same colour as the line**, so it is obvious which column is on the chart. A vertical cursor marks the timestamp selected in the table, so both halves of the screen point at the same moment. On short terminals (24 rows or fewer) it stays hidden by default: a table reduced to a couple of rows can no longer be navigated. `v` forces it down to 18 rows; below that it cannot be shown at all.

It follows the same rules as the other native outputs:

- **`—` means the value is absent, not zero.** Fields the source generation never had, missing samples and intervals where no delta can be taken are never filled with 0.
- **`!` before a timestamp marks a discontinuity**, `R` a restart, `C` a comment. What happened between two samples was not observed, so points are not joined.
- **The graph line breaks for the same reason.** Gaps and discontinuities split it into separate segments that are never bridged, and a missing sample is never plotted as 0. When the line is split, the count is stated in words rather than left to colour alone. When nothing can be plotted at all, the reason is printed (`unsupported_by_source: 120`) instead of an empty pair of axes.
- Timestamps use the `--timezone` basis (local by default), and the screen says so.

The TUI needs an interactive terminal. Piped or redirected, it tells you to use `resarch show` / `resarch sar` instead.

### The MEMORY tab's comparison graph

The MEMORY tab opens on a comparison of memory utilisation (`A_MEMORY / - / メモリ使用率の比較 (percent)`): three estimates drawn against the same 0–100 % axis. They differ only in what they count as free.

| Line | Legend | Column | Counted as free |
|---|---|---|---|
| cyan | buffers/cache を除く (excluding buffers/cache) | `memused_nocache_pct` | `kbmemfree` + `kbbuffers` + `kbcached` |
| green | MemAvailable 基準 (based on MemAvailable) | `memused_pct` | `kbavail` (the kernel's `MemAvailable`) |
| magenta | buffers/cache を含む (including buffers/cache) | `memused_withcache_pct` | `kbmemfree` alone |

- The gap between "including" and "excluding" is buffers and cache. The gap between "excluding" and the MemAvailable line is the effect of shared memory, tmpfs, reclaimable slab and the like (see [Reading memory usage](#reading-memory-usage)).
- The legend sits on the graph's top border as a coloured marker and a name. On a narrow screen the names shorten (除く / MemAvailable / 含む); if they still do not fit, the legend moves to the left end of the bottom border.
- A series without values gets no line, and its legend entry is greyed out with the reason: 記録なし (not recorded) when the generation never had the field at any timestamp, 値なし (no values) when missing samples or other reasons are mixed in. Files that do not record `kbavail` — sysstat 10.1.5 on RHEL / CentOS 7, for example — show the MemAvailable line as not recorded. Nothing is drawn as 0 or filled in from another value. If none of the three has a value, the tab opens on the graph of the first column that does.
- In the table header, each column with a line on the graph takes that line's colour. The four new columns (`kbmemused_nocache`, `memused_nocache_pct`, `kbmemfree_withcache`, `memused_withcache_pct`) follow `memused_pct`, so their values can be read per timestamp without scrolling sideways.
- The comparison graph heads the `[` / `]` cycle: `]` moves on to single-column graphs and `[` comes back. In the `c` popup, moving the cursor and applying switches to that column's own graph; applying without moving it keeps the comparison.
- **The four new columns are explained below the table, just above the key hint.** Each column gets one line that says what it counts as free (or used) and gives the formula, followed by a caveat: all of them treat buffers/cache wholesale, and the accurate free figure is `kbavail`. The column names are written as in the table header, and a column with a line on the graph takes that line's colour. Columns hidden with `c` are not explained.
  - When the screen is too narrow, every line switches to its short form together (full and short forms are never mixed). If even the short forms do not fit on one line each, the notes collapse into a single line pointing to the `?` reference, rather than cutting a description off from its column name.
  - On a low terminal where the notes would leave fewer than five table rows, they collapse into a one-line summary, and disappear if even that does not fit. The table always takes priority.
- The `?` reference explains the three lines and gives the formula of each new column.

## Reading memory usage

sysstat has changed how it computes `kbmemused` and `%memused` three times, so the same moment on the same host can look very different depending on which `sar` printed it.

| sysstat | `kbmemused` | Notes |
|---|---|---|
| Before 11.7.4 (11.6.4 / 11.4.10 on the stable branches), including 10.1.5 on RHEL / CentOS 7 | `MemTotal − MemFree` | Buffers and page cache count as used, so `%memused` tends to sit in the 90s |
| 11.7.4 to 12.7.7 | `MemTotal − MemFree − Buffers − Cached − Slab` | Changed to be consistent with free(1) and top(1) |
| 12.7.8 and later | `MemTotal − MemAvailable` (`kbavail`) | What reSARch's `kbmemused` / `memused_pct` follow (for compatibility output, see below) |

**A small `kbmemfree` is not a problem in itself.** Linux puts idle memory to work as page cache and reclaims it when something needs it, so `kbmemfree` at a few percent of the total does not, on its own, mean memory is short. It is also why an old `sar`'s `%memused` stays in the 90s.

**The accurate estimate of free memory is `kbavail` (`MemAvailable`).** The kernel added it in 3.14. Its documentation (`filesystems/proc.rst`) defines it as "An estimate of how much memory is available for starting new applications, without swapping", calculated from `MemFree`, `SReclaimable`, the size of the file LRU lists and the low watermarks in each zone. sysstat records it from 11.5.3. The RHEL 7 kernel (3.10.0) provides it as well, but RHEL 7's sysstat 10.1.5 does not record it. procps-ng's `free` added an `available` column based on `MemAvailable` in 3.3.10, and since 4.0.1 reports `used` as total − available (its `cache` today is `Cached` plus `SReclaimable`, so it will not match `kbcached`).

**Excluding buffers and cache is an approximation.** Before `MemAvailable`, free memory was commonly estimated by adding up free and cached. The kernel commit that introduced `MemAvailable` (34e431b0) says the practice "was fine ten years ago, but is pretty much guaranteed to be wrong today": `Cached` includes memory that cannot be freed as page cache — shared memory segments, tmpfs, ramfs — and it leaves out reclaimable slab. On a host that uses a lot of shared memory or tmpfs, the figure that excludes buffers and cache therefore understates usage (it overstates free memory); a large reclaimable slab pushes it the other way.

reSARch's own output keeps these apart as separate columns:

| Column | Formula | Use it for |
|---|---|---|
| `kbavail` (kB) | the kernel's `MemAvailable` | judging headroom — start here |
| `memused_pct` (%) | `(kbmemtotal − kbavail) / kbmemtotal × 100` | the same |
| `kbmemused_nocache` (kB) | `kbmemtotal − kbmemfree − kbbuffers − kbcached`, 0 if negative | following the trend where `kbavail` is not recorded (approximate) |
| `memused_nocache_pct` (%) | `kbmemused_nocache / kbmemtotal × 100` | the same |
| `kbmemfree_withcache` (kB) | `kbmemfree + kbbuffers + kbcached`, not capped at the total | the same |
| `memused_withcache_pct` (%) | `(kbmemtotal − kbmemfree) / kbmemtotal × 100` | matching `%memused` from a `sar` older than 11.7.4 |

- The four new columns use only inputs that old generations record as well, so they have values where `kbavail` is missing, and their formulas never change with the generation. That is also why they do not subtract `Slab` as sysstat 11.7.4–12.7.7 does: `kbslab` is absent before 11.1.3.
- Where `kbavail` is not recorded, native output leaves `kbavail`, `kbmemused` and `memused_pct` empty (`—`). They are not filled in from the new columns, which measure something else. `detect` and `summarize` do not use the new columns either; without `kbavail`, they report those memory checks as not evaluated.
- Compatibility output (`sar -r`, `sadf`) keeps upstream's formulas and carries none of the new columns. For a file without `kbavail`, it uses `kbmemfree` in its place, as upstream's `sadf -c` does, so `%memused` there equals the pre-11.7.4 figure (buffers and cache included). `--sar-profile sysstat-10.1.5-el7` output uses that formula to begin with.

## Finding what went wrong

`resarch detect` takes a file and tells you **when** and **what** looks off, without needing you to know what to look for. It runs three views over every series it can evaluate — fixed conditions on values whose meaning is established, deviation from the file's own median and MAD, and level changes between adjacent windows — and groups what fires into episodes.

What it will not do is dress up a guess as a measurement:

- **No confidence percentages.** A calibrated probability cannot be built from one host's 144 samples. You get an ordinal investigation priority and, separately, how much evidence backed it.
- **It says when its own yardstick is suspect.** The comparison basis comes from the input itself, so if the anomaly dominates the file, the basis moves with it. When the median itself satisfies a fixed condition, the report says the deviation check for that series cannot be trusted.
- **`MAD = 0` is not divided by epsilon.** A series that barely moves has unmeasurable spread, which is reported as such rather than turned into an enormous score.
- **Sampling is not disguised as duration.** Three high readings are "high across 3 samples spanning 20 minutes", never "high for 20 minutes". What happened between samples was not observed.
- **"Not evaluated" is not "nothing found".** Every series it could not assess is listed with the reason.

The default text report helps you choose what to investigate first. It shows up to five headline series in priority order, with up to three episode times per series. Times are selected by priority too, so a later important event is not hidden behind earlier minor ones. Each series includes the observed values and reasoning for a representative finding. Up to three standing findings are shown in priority order.

Nothing the report owes you is dropped. Each group keeps the times, the priority of each occurrence and the sample counts; episodes where **another series fired at the same time** are marked `+N 系列`, since that overlap is the clue that something happened rather than drifted. Where the basis came from, the evaluation coverage and the caveats specific to a series all stay. What a finding does **not** establish moves to the end of the report, listed once per metric instead of once per episode.

Omitted counts are explicit; omitted does not mean absent. Use `--verbose` for all episodes and their detailed evidence and interpretations. `--format json` and `--format ndjson` apply no display limits and always carry every field. Inspect the selected series around those times with `show --activity ... --from ... --to ...`.

## Output language

`detect` reports in English or Japanese. The language is resolved in this order:

1. `--lang ja|en`
2. `RESARCH_LANG`
3. `LC_ALL` → `LC_MESSAGES` → `LANG` (`C` / `POSIX` / `C.UTF-8` settle on English)
4. `LANGUAGE` (the GNU list, first supported entry wins)
5. The local time zone — Japanese if it is `Asia/Tokyo`
6. English

**Locale beats the time zone.** A locale is a statement about which language you want to read; a time zone only says where you are. Someone working in Japan with `LANG=en_US.UTF-8` has told the machine which they prefer, and the time zone does not override that. For the same reason `LC_ALL=C` settles on English before `LANGUAGE` is consulted — scripts rely on that locale for deterministic output.

JSON and NDJSON keep English keys and stable enum values whatever the language; only the human-readable sentences follow it.

## Charting detected anomalies as SVG

```bash
resarch detect sa13 --svg-dir charts
resarch detect sa13 --svg-dir charts-15m --svg-context 15m
resarch detect sa13 sa14 --activity cpu,disk --from 09:00 --to 10:00 \
  --svg-dir charts-window --svg-context 1h --format json > detections.json
```

Each SVG covers a detected **host, resource and metric**, zoomed to the detection and its surrounding context. The default adds 30 minutes on each side; use `300s`, `15m`, `1h` or `0` to change it. Overlapping windows for the same metric are merged; distant detections and separate boot segments remain separate. Charts label times with the `--timezone` basis (local by default), and `index.json` records the basis actually used in `timezone`. They may include input samples outside `--from` / `--to` as context. Chart options do not change detection thresholds or baseline selection.

- SVG files show detection ranges, observations and applicable thresholds or comparison baselines. Missing samples and discontinuities break the plotted line.
- `index.json` maps filenames to hosts, resources, metrics, windows and findings.
- `report.json` contains the ordinary JSON assessment, including evaluation limits.

Choose a new output directory. The ordinary text / JSON / NDJSON report still goes to stdout. With no detections, only the empty index and assessment are saved. Incomplete `--lenient` results exit nonzero and mark the index as `partial` with input diagnostics. Standing findings get separate background charts so they do not expand local detection windows. A level-shift boundary is not presented as a confirmed event time.

## Converting an old file

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # override the assumed HZ
```

Old headers do not record the tick frequency. reSARch uses **USER_HZ=100** by default, matching Linux on common architectures and the direct-reading path. This is the unit of `/proc/stat`, independent of the kernel's `CONFIG_HZ`. Only `-O hz=` overrides it for an input with a known different tick frequency; wall-clock gaps and suspend never change it. The chosen value and its source are reported on stderr.

An explicit HZ must fit `1..=4294967295`; larger values are rejected instead of truncated, and `hz=0` keeps the default. Unknown activities keep their original item bytes. In 0x2173 files, per-record counts preserve changes across restarts. A restart without a known positive CPU count cannot be represented in the current format and is rejected.
