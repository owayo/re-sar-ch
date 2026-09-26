---
name: resarch
description: >-
  Analyze sysstat sa binary logs (/var/log/sa/saXX) without sar, sadf, or libc.
  Use for performance incident investigations, resource anomalies, sa files,
  sar logs, sadf output, identifying the sysstat version that wrote a file,
  files rejected by sar, or reproducing CentOS 7 sarDD reports.
  Provides detect for anomaly triage, show for structured data, summarize for
  period statistics, compare for host comparisons, identify for file versions,
  and sar/sadf compatibility output. Reads legacy formats and files collected
  on 32-bit or big-endian systems. The sysstat-10.1.5-el7 profile reproduces
  RHEL/CentOS 7 sar text through sa2sar and the sar compatibility entry point.
allowed-tools: Bash(resarch:*)
---

# resarch

Read the **binary logs** sysstat writes to `/var/log/sa/saXX`, without `sar`,
`sadf`, or a C library. resarch knows all format generations shipped by sysstat
and resolves structure layouts from **the writer's ABI**, so Linux logs can be
read on macOS and Windows.

## Output language (required)

**Always pass `--lang en` when running `detect`, `show`, `summarize`, `compare`,
or `tui` as an agent.** Read command output in English to reduce token usage.
Include it for JSON and NDJSON too: these formats can contain explanatory text.
Do not rely on environment variables or the host's language settings.
Respond to the user in their requested language.

Do not add `--lang` to `info`, `identify`, `sar`, `sadf`, `sa2sar`, `skill-install`,
or the implicit sar entry point: they do not support it.

## Choose a command

| Task | Command |
|---|---|
| **Investigate an incident: find when and what looked unusual** | `resarch detect <file> --lang en` |
| Plot context around detections as SVG | `resarch detect <file> --lang en --svg-dir charts --svg-context 30m` |
| Extract structured data for agent analysis | `resarch show <file> --lang en --format ndjson` |
| Compute period averages, p95, and bottleneck assessments | `resarch summarize <file> --lang en --format json` |
| Compare hosts over a shared time window | `resarch compare --lang en --host a=<f1> --host b=<f2>` |
| **Identify which sysstat versions could have written a file** | `resarch identify <file>...` |
| Inspect the format generation, ABI, and recorded activities | `resarch info <file>` |
| Feed existing tools or scripts with upstream-compatible output | `resarch -u -f <file>` / `resarch sadf -j <file>` |
| Convert a legacy file for other tools | `resarch sadf -c <file> > out` |
| Save all activities from an sa binary as sar text | `resarch sa2sar sa13 -o sar13` |
| Reproduce sarDD written by sa2 on RHEL/CentOS 7 | `resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13` |

`sa2sar` includes averages, RESTART, and COMMENT records. It uses the recorded
source time by default; `--utc` selects UTC. Omitting `-o`, or using `-o -`,
writes to stdout. Existing destination files are never overwritten, and a
failure does not leave a partial file at the destination.

**The default compatibility profile is upstream sysstat 12.8.0.** To compare
against sarDD from RHEL/CentOS 7 (the sysstat 10.1.5 el7 package), pass
`--sar-profile sysstat-10.1.5-el7`. This reproduces el7 columns (`%vmeff` in `-B`,
`rd_sec/s ... svctm` in `-d`, and the `-R` block), CPU `all` calculations, and
integer division in averages for byte-for-byte matching.
The profile reads only `format_magic` 0x2171. It is never selected automatically:
the header identifies `sadc`, not the version or patches of the `sar` used by
`sa2`. For `-R` on ppc64 hosts, use `--sar-page-size 65536`.

**A file rejected by sar may still be readable.** sar rejects a noncurrent
`format_magic`; resarch directly reads 28 registered formats: 22 legacy
monolithic formats including `0x2168`, sysstat 2.2's `0x015d`, and
`0x1170` / `0x2170` / `0x2171` / `0x2173` / `0x2175`.
Legacy values absent from the file or with unknown units are unavailable;
some arrays, including per-CPU IRQ data, are not decoded.
`exact=true` confirms scanning to the exact end of the file, not agreement on
all metrics or with historical sar output.
32-bit and big-endian inputs are supported, but legacy formats without a
recorded long width default to 8 bytes, and sysstat 2.2 defaults to little-endian.
The CLI has no ABI override; use the library's `OpenOptions` for other inputs.

## Identify the sysstat version

```bash
resarch identify /var/log/sa/sa07
resarch identify sa*.bin --format json      # Machine-readable output
```

Even an unreadable generation returns identification results with **exit code 0**.
An unreadable format is not a command failure; failure to open the file is.
Unlike `info`, which requires a decodable header, `identify` reads **only the
first 1 KiB** to determine what the file is.

Do not confuse these columns:

- **`SYSSTAT` is the version range inferred from the magic**, not the writer's
  exact version. One magic can span multiple versions; for example, `0x2169`
  covers four releases from 6.1.3 through 7.0.4.
- **`RECORDED` is the version stored in the file itself.** Only generations with
  `file_magic` (starting at `0x216f`) have it. Earlier files show `-`, meaning
  the version was not recorded, not that reading it failed.

A header-layout mismatch in `NOTE` means the magic is known but the header is
corrupt or truncated. An unknown-format-magic note means the generation is
not yet recognized.

## Start with detect

Use this as the entry point when something happened on a host, even if you
**do not yet know which metrics to inspect**.

```bash
resarch detect /var/log/sa/sa07 --lang en
resarch detect sa07 --lang en --format json    # Full structured fields for agents
resarch detect sa07 --lang en --verbose        # Per-detection details and interpretations
resarch detect sa07 --lang en --min-priority investigate   # Minimum priority
resarch detect sa07 --lang en --from 09:00 --to 10:00      # Report window only; see below
```

Three detection routes evaluate every eligible series. Detections close in
time are grouped into **episodes**.

### Output language

`detect` keys and enum values are always English, but explanatory text depends
on the language setting. Follow the required rule above: combine
`--format json` / `--format ndjson` with `--lang en`.

### Text is a summary; agents should use JSON

Default `text` helps choose what to inspect first. Episodes are grouped by
headline series, showing up to five series in priority order. Each series
shows up to three timestamps and sample counts in priority order, with observed
values and a rationale for a representative detection. Up to three background
findings are shown in priority order.
Omitted counts are explicit. **Omitted output does not mean no detections.**
The summary also marks episodes with detections in additional series; do not
ignore these overlaps when investigating a possible event.

Detailed evidence and possible interpretations are omitted except for the
representative evidence described above.
**JSON and NDJSON contain all fields regardless of `--verbose`.** Each episode
has its own entry; `headline_series` / `headline_metric_label` identify its
headline detection.

The summary still includes:

- The source of the comparison baseline, in the header and closing caveats.
- **Evaluation coverage:** series that could not be evaluated and why.
- Series-specific caveats, including a potentially biased baseline.
- **What cannot be concluded from this result**, collected once per metric
  at the end rather than repeated in every episode.

Use `--verbose` to inspect all episodes individually in text.
Follow up on a series and time range with
`show --lang en --activity ... --from ... --to ...`.

| Route | What it evaluates |
|---|---|
| Absolute level | Fixed conditions on metrics with established meaning, such as direct reclaim or sustained high `%util` |
| Reference-distribution deviation | Deviation from the input file's own median and MAD |
| Temporal change | Level differences between adjacent windows |

### Rules for interpreting detect output

Treat the caveats as evidence constraints, not decoration.

- **No confidence percentages are provided.** A single host's 144 samples
  cannot establish calibrated probabilities. **Investigation priority**
  (ordinal: informational / watch / investigate) and **evidence sufficiency**
  are separate fields. Never multiply them into a single score.
- **A potentially biased baseline makes deviation assessments unreliable.**
  The baseline comes from the input itself: if unusual behavior dominates
  the file, it shifts the baseline. If the median itself meets a fixed
  condition, do not rely on deviation values as evidence.
- **Three samples spanning 20 minutes do not mean 20 continuous minutes.**
  sar samples are discrete; behavior between samples is unobserved.
- **Two firing routes do not mean twice the evidence.** The three routes are
  correlated; a drop in `%idle` can trigger all three.
- **A boundary between different levels is not the exact change time.**
  It is the split selected between the adjacent comparison windows.
- **Background findings lack timing clues; they are not necessarily
  unimportant.** Conditions covering almost the entire input, such as swap
  usage throughout the day, belong here.
- **Unevaluated series do not mean no anomalies.** Read their reasons before
  concluding that there is no problem.
- If the report says edge samples lack before/after comparisons and were
  excluded from change detection, changes at those file boundaries were
  not evaluated by that route.

## Plot detected regions

`detect --lang en --svg-dir <new-directory>` saves SVGs by detected host, boot
segment, resource, and metric, with 30 minutes of context on either side.
Use `--svg-context 300s/15m/1h/0` to change the context width.
Overlapping windows within a series are merged; separated detections get
separate SVGs.
Times follow `--timezone` (local by default); `index.json` records the resolved
zone in `timezone`. Available context outside `--from` / `--to` is retained.
Plotting does not change detection conditions.
`index.json` maps files to detections; `report.json` includes the report and
reasons for unevaluated series. No detections produces an empty index and no
SVGs. Partial input is marked `partial` in the index and exits nonzero.
The normal report still goes to stdout. Existing directories are not overwritten.

## Extract structured data

Use NDJSON or JSON for agent analysis.

```bash
resarch show sa07 --lang en --format ndjson --activity cpu,disk
resarch show sa07 --lang en --format ndjson --values both     # Separate raw and derived namespaces
resarch show sa07 --lang en --format json --from 09:00 --to 18:00
```

`--values` accepts `raw` (raw cumulative counters), `derived` (rates and
percentages; default), or `both`.

### Missing is not zero

Native output reports why a value is unavailable.

| Quality | Meaning |
|---|---|
| `unsupported_by_source` | **The source generation has no such field.** This is not zero. |
| `missing_in_sample` | The field exists, but its value is unavailable in this record. |
| Discontinuity (`restart` / `item_replaced` / ...) | No valid delta can be computed, so no rate was calculated. |

Treating missing values as `0` silently corrupts averages, p95, and threshold
assessments. **An empty value in native output is not zero.**

Raw `u64` values are **decimal strings** to avoid JavaScript rounding above 2^53.

## Summarize periods and compare hosts

```bash
resarch summarize sa07 sa08 --lang en --format json      # Aggregate multiple files
resarch compare --lang en --host web1=web1/sa07 --host web2=web2/sa07
```

Assessments include **rule IDs and observed evidence**. Thresholds, duration,
required metrics, missing-data behavior, and rule versions remain in the output
so the reasoning can be reconstructed later.

`compare` uses the **intersection** of the hosts' observation windows.
Never treat a window without observations from one host as zero.

## sar / sadf compatibility output

Use compatibility output for existing tools and scripts. Full output matching
against upstream expectations has been verified in 21 cases.

```bash
resarch -u -f sa07                       # No subcommand means sar mode
resarch -r -f sa07
resarch -n DEV,EDEV -f sa07
resarch -u -P ALL -s 09:00:00 -e 18:00:00 -f sa07
resarch -u -i 600 -f sa07                # Sample at 10-minute intervals
resarch -I --int=0,LOC -f sa07           # Select interrupts by number or name
resarch -A -f sa07 1 1                   # Positional interval / count
resarch sar -A -f sa07                   # Explicit compatibility entry point
resarch sadf -j sa07                     # JSON
resarch sadf -d sa07 -- -d               # Disk statistics in semicolon-delimited DB format
resarch sadf -r sa07 -- -b               # Raw counters
```

**sar quirks are intentional:**

- `-I` does not take a numeric argument.
- `-P ALL` and `-P all` are different.
- **`-h` means `--pretty --human`, not help.**
- The **first record matching `-s` is consumed as the previous sample
  (baseline)** and is not displayed.

**`--sar-profile sysstat-10.1.5-el7` selects RHEL/CentOS 7 sar syntax and output.**
Here `-h` is help, `-R` exists, and `-I` accepts `SUM`, `ALL` (first 16 interrupts),
`XALL`, or an interrupt number. Options absent from 10.1.5, such as `--dec=`,
`-x`, `-z`, and `-r ALL`, produce usage errors.
`sadf` has no profile option and rejects it.

```bash
resarch --sar-profile sysstat-10.1.5-el7 -A -f sa07      # Same as CentOS 7 sar -A
resarch --sar-profile sysstat-10.1.5-el7 -u -P ALL -f sa07
```

**Compatibility output fills absent fields with zero**, matching upstream's
zero-initialized structures. Use native output to preserve missingness.

**Upstream sar formatting environment variables also apply** to `sar`,
`sa2sar`, and `show --lang en --format sar`. Use them to reproduce source-host
formatting. Leaving both unset preserves the default behavior.

| Variable | Value | Effect |
|---|---|---|
| `S_TIME_FORMAT` | Exactly `ISO` | Changes the banner date from `MM/DD/YY` to `YYYY-MM-DD` |
| `S_REPEAT_HEADER` | Digits only, greater than 0 | Repeats column headers every N lines when stdout is not a terminal |

Terminal output instead uses the window height (`rows - 2`). Neither variable
applies to `sadf`, matching upstream.

**`sar -A` and `sa2sar` repeat headers even without these variables.** For
activities using a CPU bitmap, upstream counts each sample as
`count_bits(cpu_bitmap)` lines. `-A` and `-P ALL` fill the whole bitmap, so one
sample counts as 8200 lines regardless of the actual CPU count. Eleven samples
exceed the default 86400-line limit. Account for this when counting output
records from header occurrences.

### Device names

`sar -d` emits names such as `dev8-0`. Upstream resolves `major:minor` through
the **execution host's** `/dev` and `/sys`, which can misidentify devices in
logs collected elsewhere. `--dev=` also matches `dev<major>-<minor>`.

## Convert legacy files

```bash
resarch sadf -c sa07 > sa07-current        # 0x2171 / 0x2173 -> 0x2175
resarch sadf -c sa07 -O hz=250 > out       # Override the assumed HZ
```

Converted binary data goes **only to stdout**; progress and warnings go to stderr.

Legacy headers do not store HZ, so direct reading and conversion default to
USER_HZ=100. This differs from `CONFIG_HZ`; do not infer it from wall-clock deltas.
Use `-O hz=` only when the source tick frequency is known. The chosen value and
its source are reported on stderr.

## Pitfalls

Native commands interpret `--from` / `--to` values in `hh:mm[:ss]` using
**`--timezone`**, also used for display (local by default).
Use `--timezone local|utc|<IANA-name>` or `--utc` as shorthand for UTC.
Ten-digit epoch seconds are independent of the timezone; structured output's
`start_epoch` / `end_epoch` remain epoch seconds.

| Symptom or task | Cause and action |
|---|---|
| Times are not UTC or differ from older output | Native output defaults to the local timezone. `--utc` (= `--timezone utc`) restores `...Z` timestamps. `info`, `identify`, and compatibility entry points (`sar`, `sadf`, `sa2sar`) have no `--timezone`. `report_timezone` in `detect --lang en --format json` / `ndjson` records the resolved zone. |
| `--from` / `--to` behave differently across commands | `show` filters **displayed rows**; `summarize` / `compare` filter **the aggregation period**; `detect` filters **only the report window**, keeping the baseline input unchanged. detect needs context outside a narrow investigation window. |
| `summarize --lang en --from` produces no results | The first matching record is consumed as the previous sample. A window with only one record has no interval to aggregate; stderr explains why. |
| `hh:mm:ss` filtering continues after the first day | Intended behavior: time-of-day filters apply **each day**, as with `sar -s` / `-e`. |
| `sar -A` omits an expected activity | Upstream also omits activities whose magic differs from the current version. Check with `resarch info <file>`; native `show` output can include them. |
| Inspect per-CPU interrupts | Use `show --lang en --activity irq --irq-cpus`. The `cpu` dimension contains `all` and CPU numbers; only `all` is available if the legacy file lacks per-CPU data. |
| Produce SVG graphs | `sadf -g sa07 -- -u -P ALL > cpu.svg` uses a native renderer with shared computed values. `-O autoscale,packed,customcol` and PCP (`-l`) are unsupported and rejected. |
| `--dev=` and similar filters do not work in sadf | Item-name filters currently apply only to sar (known limitation). |
| Corrupt files stop processing | `--lenient` skips recoverable corruption with diagnostics. The default `--strict` rejects suspicious data. |
| Large files are slow | Restrict `--activity`; unselected activities are skipped without decoding (measured 2.8x faster than decoding all). Use `--jobs` to set file-processing parallelism. |

## Output conventions

- **Data goes to stdout; diagnostics go to stderr.** Native warnings are not
  mixed into compatibility output.
- An unreadable input causes a **nonzero exit**, even when partial results exist
  (see the format-identification exception under `identify`).
- `-f` also accepts a directory. Daily files named `saDD` / `saYYYYMMDD` under
  `SA_DIR` (default `/var/log/sa`) are selected by **mtime**.

## Example investigation

```bash
# 1. Identify the file, including unsupported generations, and inspect metadata
resarch identify /var/log/sa/sa07
resarch info /var/log/sa/sa07

# 2. Find candidate times and metrics
resarch detect /var/log/sa/sa07 --lang en

# 3. Inspect the detected time range in upstream-compatible format
resarch -u -P ALL -s 16:00:00 -e 17:00:00 -f /var/log/sa/sa07

# 4. Extract values for analysis
resarch show /var/log/sa/sa07 --lang en --activity cpu,disk,memory \
  --from 16:00 --to 17:00 --format ndjson --values both

# 5. Compare with the previous day
resarch compare --lang en --host d06=/var/log/sa/sa06 --host d07=/var/log/sa/sa07
```

**Do not skip step 2.** detect reports unevaluated series as well as detections,
so you can account for coverage gaps before narrowing the investigation.
