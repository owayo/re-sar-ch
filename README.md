<p align="center">
  <img src="docs/images/app.png" width="128" alt="reSARch">
</p>

<h1 align="center">re<strong>SAR</strong>ch</h1>

<p align="center">
  A standalone CLI for sysstat sa files. Detect anomalies, chart them as SVG, and produce sar / sadf reports on Linux, macOS and Windows.
</p>

<h3 align="center">Supported Platforms</h3>

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

Inspect `sa` files collected on Linux from your own PC, without installing `sar` or `sadf`
on the machine doing the analysis. Start with Installation and Quickstart; use the contents
below to look up commands, supported formats, and verification details.

## Contents

- [Features](#features)
- [Requirements](#requirements)
- [Installation](#installation)
- [Quickstart](#quickstart)
- [Usage](#usage)
- [CLI reference](#cli-reference)
- [Use it from an AI agent](#use-it-from-an-ai-agent)
- [Supported Formats](#supported-formats)
- [Why](#why)
- [How the output is verified](#how-the-output-is-verified)
- [What "sar compatible" means here](#what-sar-compatible-means-here)
- [Design notes](#design-notes)
- [Development](#development)
- [Release](#release)
- [License](#license)

## Features

- `sar` / `sadf` compatible text, JSON, CSV, and XML reports
- Interactive tables and graphs in a terminal UI
- Anomaly detection, period summaries, host comparisons, and SVG charts
- Readers for 28 registered formats from sysstat 2.2 through 12.8, with [legacy limitations](#centos-345-and-earlier-formats)
- [Automated collection and verification](#verify-all-available-official-sysstat-releases) using distribution and official sysstat release images

## Requirements

Runs on Linux, macOS, and Windows. Supply an `sa` file collected by sysstat on Linux.
The binary analyses saved files. To collect new data, use sysstat on Linux or the bundled
[collection scripts](docs/format/06-live-matrix.md). The TUI requires an interactive terminal;
building from source requires a Rust toolchain.

## Installation

### Homebrew (macOS/Linux)

```bash
brew install owayo/re-sar-ch/re-sar-ch
```

### winget (Windows)

```powershell
winget install owayo.reSARch
```

### From Source

A Rust toolchain is required.

```bash
cargo install --git https://github.com/owayo/re-sar-ch --locked --bin resarch
```

`--bin resarch` installs the CLI alone; the repository also contains `xtask`, a
development-only binary.

To build from a local checkout, use `make`. It needs [mise](https://mise.jdx.dev/), which
installs the Rust toolchain pinned in `mise.toml`. Without mise, add `SYSTEM_TOOLS=1` to
build with the Rust toolchain on your PATH.

```bash
git clone https://github.com/owayo/re-sar-ch.git
cd re-sar-ch
make setup
make install
```

`make install` builds a release binary, installs it into `/usr/local/bin`, and then writes
the reSARch skill for Claude Code and Codex to `~/.claude/skills/resarch` and
`~/.codex/skills/resarch`. Use `make install-bin` for the binary only. `INSTALL_PATH`
changes the destination and `SKILL_TARGETS` chooses the agents:

```bash
make install INSTALL_PATH="$HOME/.local/bin"   # install without sudo
make install SKILL_TARGETS=claude              # the Claude Code skill only
make install SKILL_TARGETS=                    # no skills
```

On Windows, use `cargo install --path . --locked --bin resarch`.

### From GitHub Releases

Platform binaries are available from [Releases](https://github.com/owayo/re-sar-ch/releases).

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

Download `resarch-x86_64-pc-windows-msvc.zip` from [Releases](https://github.com/owayo/re-sar-ch/releases), extract it, and add its directory to PATH.

> winget registers `resarch` on PATH automatically. After installation, open a new terminal so the updated PATH takes effect.

## Quickstart

After installation, replace `sa01` with the path to your file. Copy it from the collecting
host if you are analysing it on another machine.

```bash
resarch info sa01               # Inspect the format and recorded activities
resarch -u -f sa01              # Display CPU utilisation
resarch detect sa01             # Find times and metrics to investigate
```

An **activity** is a kind of statistic, such as CPU or memory; an **item** is an individual
CPU, device, or other measured resource. Use `resarch --help` or `resarch detect --help`
to look up commands and options.

## Usage

### As a drop-in for `sar`

Omit the subcommand to read saved files with `sar`-compatible arguments. Output defaults
to the sysstat 12.8.0 profile, regardless of the input file's generation. See
[compatibility](#what-sar-compatible-means-here) for supported options and output profiles:

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

The quirks are reproduced deliberately: `-I` takes no number, `-P ALL` differs from
`-P all`, `-h` means `--pretty --human` rather than help, and the first record matched by
`-s` is consumed as the baseline rather than displayed.

### Saving an sa binary as sar text

```bash
resarch sa2sar sa13 -o sar13             # All activities, averages, restarts and comments
resarch sa2sar sa13 --utc -o sar13-utc   # Use UTC timestamps
resarch sa2sar sa13                     # Write to stdout for piping
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13   # what a RHEL/CentOS 7 host's sa2 writes
```

The default is equivalent to `sar -A -C -t -f sa13`, using timestamps recorded by
the source host. Omit `-o` or use `-o -` for stdout. Existing destination files
are never overwritten; failed conversion leaves no partial destination file.

`--sar-profile sysstat-10.1.5-el7` renders the text the way the host's own `sa2` does
on RHEL / CentOS 7 — the columns, the arithmetic and the rounding of averages all
follow sysstat 10.1.5 as Red Hat ships it. Use it to generate a `sarDD` that `sa2` has not
written. Reproduction depends on the supported RPM version, page size, and device names.
See [Reproducing RHEL / CentOS 7's `sar`](#reproducing-rhel--centos-7s-sar).
Old and big-endian inputs are read directly. CI compares CLI-generated files
against five upstream golden reports covering three old versions, a current
format and big-endian data. Only disk names unavailable on the reading host
use the existing comparison mask.

### Its own commands

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

For `summarize` and `compare`, `--from` / `--to` narrow **the aggregation period itself**:
samples outside the range enter neither the mean, the p95, nor the delta totals, and the
period bounds shrink with them. `detect` reads the same two options differently — they
narrow what gets reported, not the material its comparison basis is built from.
Native commands interpret `hh:mm[:ss]` in **the same timezone they display**, local by
default; 10-digit epoch seconds are also accepted. `compare` JSON contains `comparisons`
and `skipped_metrics`, including the hosts missing each skipped metric. IRQ rows carry a
`cpu` dimension (`all` or a zero-based CPU number); old files without CPU detail emit
only `all`.

#### Timestamp timezone

> **The default changed in v26.9.102.** Native subcommands used to print UTC; they now
> print the machine's local timezone. Pass `--utc` to get the previous behaviour back.
> See [CHANGELOG.md](CHANGELOG.md) for details.


The native subcommands (`show`, `summarize`, `detect`, `compare`, `tui`) display
timestamps in **the machine's local timezone**. `--timezone <TZ>` changes the basis and
accepts `local` (the default), `utc`, or an IANA name such as `Asia/Tokyo`. `--utc` is an
alias for `--timezone utc`; giving both is an error.

```bash
resarch summarize sa01                        # 2026-08-31 15:10:01+09:00
resarch summarize sa01 --utc                  # 2026-08-31 06:10:01Z
resarch summarize sa01 --timezone Asia/Tokyo  # Japan time on any machine
```

The same basis interprets `hh:mm[:ss]` in `--from` / `--to`, so the 09:00 on screen and
the `--from 09:00` you type never disagree. 10-digit epoch seconds stay timezone
independent. Timezones are named by their IANA name (`Asia/Tokyo`), falling back to a
numeric offset (`+09:00`) where no IANA name can be determined. Abbreviations like `JST`
are never used: they collide, and on a DST transition day they cannot tell apart the two
occurrences of the same wall clock.

Machine-readable formats (json / ndjson / csv) keep `start_epoch` / `end_epoch` as epoch
seconds regardless of `--timezone`. Only `detect --format json` / `--format ndjson` carry
`report_timezone`, recording which wall clock `--from` / `--to` were read against.

`info` and `identify` have no such option: they print what the file header states, so
there is nothing to reopen in the reader's timezone. The compatible entry points
(`resarch sar`, `resarch sadf`, `resarch sa2sar`) keep upstream sysstat's own rules and
are unaffected by `--timezone` as well.

### Browsing interactively (TUI)

```bash
resarch tui sa01
resarch tui sa01 --activity cpu,disk,memory   # open with a narrowed set
```

Recorded activities become tabs; pick an item and read its time series as a table with a
graph above it.

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
| `[` / `]` | previous / next metric (only those the table shows) |
| `v` | show or hide the graph |
| `?` | key reference |
| `q`, `ctrl-c` | quit |

Inside the `c` popup, `space` adds or removes a column from the table, `a` switches
between every column and the default, and `enter` applies — the row under the cursor
becomes the graphed metric. `esc` cancels, leaving both the table and the graph alone.

For activities with many columns, such as memory, the table limits the visible columns
to keep values readable.

- **Columns that never carried a value are dropped by default.** Fields the source
  generation never had, and metrics that are not implemented, show `—` on every row and do
  nothing but push the columns you wanted off the screen.
- **Columns that do not fit the width are dropped rather than squashed.** `shift` + `←` /
  `→` scrolls to the rest, and the title shows the position (`9-18/23 列`) so it is clear
  more follow to the right. Columns held back from the table are counted separately.

`c` opens the column list. Columns without values are listed too, greyed out and
annotated as such. Widths are computed from every sample, so scrolling through rows never
makes them jump.

The graph plots one series — the selected activity, item and metric. **The header of the
graphed column is drawn in the same colour as the line**, so it is obvious which column is
on the chart. A vertical cursor marks the timestamp selected in the table, so both halves
of the screen point at the same moment. On short terminals (24 rows or fewer) it stays hidden by default: a table reduced
to a couple of rows can no longer be navigated. `v` forces it down to 18 rows; below that
it cannot be shown at all.

It follows the same rules as the other native outputs:

- **`—` means the value is absent, not zero.** Fields the source generation never had,
  missing samples and intervals where no delta can be taken are never filled with 0.
- **`!` before a timestamp marks a discontinuity**, `R` a restart, `C` a comment.
  What happened between two samples was not observed, so points are not joined.
- **The graph line breaks for the same reason.** Gaps and discontinuities split it into
  separate segments that are never bridged, and a missing sample is never plotted as 0.
  When the line is split, the count is stated in words rather than left to colour alone.
  When nothing can be plotted at all, the reason is printed
  (`unsupported_by_source: 120`) instead of an empty pair of axes.
- Timestamps use the `--timezone` basis (local by default), and the screen says so.

The TUI needs an interactive terminal. Piped or redirected, it tells you to use
`resarch show` / `resarch sar` instead.

### Finding what went wrong

`resarch detect` takes a file and tells you **when** and **what** looks off, without
needing you to know what to look for. It runs three views over every series it can
evaluate — fixed conditions on values whose meaning is established, deviation from the
file's own median and MAD, and level changes between adjacent windows — and groups
what fires into episodes.

What it will not do is dress up a guess as a measurement:

- **No confidence percentages.** A calibrated probability cannot be built from one host's
  144 samples. You get an ordinal investigation priority and, separately, how much
  evidence backed it.
- **It says when its own yardstick is suspect.** The comparison basis comes from the input
  itself, so if the anomaly dominates the file, the basis moves with it. When the median
  itself satisfies a fixed condition, the report says the deviation check for that series
  cannot be trusted.
- **`MAD = 0` is not divided by epsilon.** A series that barely moves has unmeasurable
  spread, which is reported as such rather than turned into an enormous score.
- **Sampling is not disguised as duration.** Three high readings are "high across 3
  samples spanning 20 minutes", never "high for 20 minutes". What happened between
  samples was not observed.
- **"Not evaluated" is not "nothing found".** Every series it could not assess is listed
  with the reason.

The default text report summarises. Episodes are grouped by the series that headlined
them, because a series that fires intermittently through a day produces one episode per
burst — a real day's file gave 111 episodes across 23 series, and listing them one by one
buried *what* is firing under 1400 lines. Each group leads with the metric, how many
episodes it accounts for and its highest priority, then the times those episodes covered.

Nothing the report owes you is dropped. Each group keeps the times, the priority of each
occurrence and the sample counts; episodes where **another series fired at the same time**
are marked `+N 系列`, since that overlap is the clue that something happened rather than
drifted. Where the basis came from, the evaluation coverage and the caveats specific to a
series all stay. What a finding does **not** establish moves to the end of the report,
listed once per metric instead of once per episode.

What the summary drops is repetition: the per-detection breakdown (observed values, basis
figures, window sizes) and the interpretation list, which is fixed per detection pattern.
`--verbose` restores every episode individually, verbatim as before. `--format json` and
`--format ndjson` are unaffected and always carry every field.

### Output language

`detect` reports in English or Japanese. The language is resolved in this order:

1. `--lang ja|en`
2. `RESARCH_LANG`
3. `LC_ALL` → `LC_MESSAGES` → `LANG` (`C` / `POSIX` / `C.UTF-8` settle on English)
4. `LANGUAGE` (the GNU list, first supported entry wins)
5. The local time zone — Japanese if it is `Asia/Tokyo`
6. English

**Locale beats the time zone.** A locale is a statement about which language you want to
read; a time zone only says where you are. Someone working in Japan with `LANG=en_US.UTF-8`
has told the machine which they prefer, and the time zone does not override that. For the
same reason `LC_ALL=C` settles on English before `LANGUAGE` is consulted — scripts rely on
that locale for deterministic output.

JSON and NDJSON keep English keys and stable enum values whatever the language; only the
human-readable sentences follow it.

### Charting detected anomalies as SVG

```bash
resarch detect sa13 --svg-dir charts
resarch detect sa13 --svg-dir charts-15m --svg-context 15m
resarch detect sa13 sa14 --activity cpu,disk --from 09:00 --to 10:00 \
  --svg-dir charts-window --svg-context 1h --format json > detections.json
```

Each SVG covers a detected **host, resource and metric**, zoomed to the detection
and its surrounding context. The default adds 30 minutes on each side; use
`300s`, `15m`, `1h` or `0` to change it. Overlapping windows for the same metric
are merged; distant detections and separate boot segments remain separate.
Charts label times with the `--timezone` basis (local by default), and `index.json`
records the basis actually used in `timezone`. They may include input samples outside
`--from` / `--to` as context.
Chart options do not change detection thresholds or baseline selection.

- SVG files show detection ranges, observations and applicable thresholds or
  comparison baselines. Missing samples and discontinuities break the plotted line.
- `index.json` maps filenames to hosts, resources, metrics, windows and findings.
- `report.json` contains the ordinary JSON assessment, including evaluation limits.

Choose a new output directory. The ordinary text / JSON / NDJSON report still goes
to stdout. With no detections, only the empty index and assessment are saved.
Incomplete `--lenient` results exit nonzero and mark the index as `partial` with
input diagnostics. Standing findings get separate background charts so they do
not expand local detection windows. A level-shift boundary is not presented as
a confirmed event time.

### Converting an old file

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # override the assumed HZ
```

Old headers do not record the tick frequency. reSARch uses **USER_HZ=100** by default,
matching Linux on common architectures and the direct-reading path. This is the unit of
`/proc/stat`, independent of the kernel's `CONFIG_HZ`. Only `-O hz=` overrides it for an
input with a known different tick frequency; wall-clock gaps and suspend never change it.
The chosen value and its source are reported on stderr.

## CLI reference

| Task | Command |
|---|---|
| Display `sar`-compatible reports | `resarch sar` (the subcommand may be omitted) |
| Produce `sadf`-compatible output or convert old formats | `resarch sadf` |
| Save a complete `sar` text report | `resarch sa2sar` |
| View tables, JSON, and other formats | `resarch show` |
| Browse interactively in a terminal | `resarch tui` |
| Detect anomalies | `resarch detect` |
| Summarise a period | `resarch summarize` |
| Compare hosts | `resarch compare` |
| Inspect headers and recorded activities | `resarch info` |
| Identify the producing sysstat version | `resarch identify` |
| Install an AI agent skill | `resarch skill-install` |

Append `--help` to a native command such as `show` or `detect` to see its options.
For `sar`, use `resarch sar --help`; for `sadf`, see [Usage](#usage) and the
[output specification](docs/format/03-output-format.md). See
[compatibility](#what-sar-compatible-means-here) for output limitations and environment variables.

## Use it from an AI agent

`resarch` ships a skill describing its own commands, so an agent knows when to reach for it
and — more importantly — how to read what comes back.

```bash
resarch skill-install claude    # ~/.claude/skills/resarch/SKILL.md
resarch skill-install codex     # ~/.codex/skills/resarch/SKILL.md
```

`make install` does both alongside the binary. The skill text is embedded in the binary, so
a release download is enough — no checkout, no network.

The skill spends most of its length on how to read `detect` output, because that is where an
agent is most likely to overclaim: the caveats are load-bearing. It also states the property
that matters most for any analysis built on top — **an empty value in reSARch's own formats
is not a zero** — and lists which `--from` / `--to` means what in which subcommand.

## Supported Formats

All 28 registered format magic values have readers. Legacy decoding limitations and
the scope of scan and output verification are described below.

| `format_magic` | sysstat versions | notes |
|---|---|---|
| `0x015d` | 2.2 | Packed selected columns; [limitations and ABI](docs/format/08-packed-legacy.md) |
| `0x115a`, `0x215a`…`0x216f` (22 formats, excluding unused `0x215c`) | 3.2.4 … 8.1.2 | Monolithic formats, including CentOS 3/4/5 and `0x2168` from 6.1.1–6.1.2; [generation evidence](docs/format/09-legacy-generations.md) |
| `0x2170` | 8.1.3 … 9.1.5 | Introduced activity arrays; upstream itself cannot convert these |
| `0x1170` | 9.0.4 (RHEL/CentOS 6.5+) | **vendor variant** — Red Hat renumbered the magic to a value no upstream release uses; `stats_io` is 80 bytes instead of 20 |
| `0x2171` | 9.1.6 … 10.2 | 8-byte file magic, no RESTART payload |
| `0x2173` | 10.3 … 11.6 | RESTART records carry a volatile-activity list that changes item counts |
| `0x2175` | 11.7 … 12.8 | self-describing layout, `extra_desc` chains, three internal variants |

`0x1170` does not exist in any upstream release. Red Hat changed the on-disk layout of
`stats_io` in RHEL 6.3 without bumping the format version, which made the new `sar`
silently misread older files; the fix in RHEL 6.5 renumbered the magic instead. Since no
upstream build ever emits a 20-byte-vs-80-byte ambiguity, the declared item size alone
identifies the variant.

### CentOS 3/4/5 and earlier formats

sysstat 3.2.4 through 8.1.2 (`0x115a` … `0x216f`) use a fundamentally different layout:
no `file_activity[]` array, no `record_header`, and — up to `0x216e` — no `file_magic` at
the head of the file either. In those generations the magic lives *inside* `file_hdr`, and
**its offset moves across generations** (4 → 36 → 32), so "read the first two bytes" does
not identify them at all.

`0x216f` (8.1.1 / 8.1.2) is a **transitional** generation: it gained `file_magic` at the head
of the file while the body was still the old layout. The switch to `file_activity[]` only
happens in `0x2170`.

All 22 monolithic formats can be read directly, including `0x2168` from sysstat 6.1.1–6.1.2.
Layouts were measured from original sources. Native samples from 24 releases pass
`exact=true` scans and rendering; independent fixtures cover both endiannesses,
32/64-bit long widths, array boundaries, and truncation.

The reader exposes supported fields present in each generation. Old per-CPU IRQ arrays are
bounded and skipped; PID-bearing pipe streams are rejected. Reports use the current column
layout and calculation rules: **successful scanning does not imply byte-identical historical
`sar` output**. Unrecorded fields and values whose units cannot be established remain unavailable.

Formats through `0x2167` do not record the long width; the default assumption is 8 bytes,
with a diagnostic. Library callers can set `OpenOptions.legacy_long_bytes = 4`.
The CLI currently has no override; use the library for 32-bit inputs that omit this width.
`0x115a` has no epoch or timezone, so its date and clock values are interpreted as UTC.
Formats through `0x216e` do not record the precise sysstat version or machine architecture.
See [legacy format specifications and verification](docs/format/09-legacy-generations.md).

sysstat 2.2 (`0x015d`) has a separate packed-column reader. Its payload endianness is not
recorded; the default is little-endian with a diagnostic, overridable through
`OpenOptions.legacy_endian`. Releases whose sources have not been recovered, such as 1.x and
3.0–3.1, are not assumed to share a neighboring layout. **Supporting all registered formats
is distinct from having verified every historical release.**

### Activity and ABI coverage

The producer ABI determines details such as integer widths and byte order.
The following are also supported, subject to the legacy limitations above:

- **All 43 activities** — CPU, memory, disk, every IPv4/IPv6 protocol, PSI, power sensors,
  filesystems, HugePages, interrupts, and the rest
- **Big-endian and 32-bit producers** — a PowerPC log opens the same as an x86-64 one
- **Files converted by `sadf -c`** — the `upgraded` marker is read and reported, and
  reSARch can perform the conversion itself
- **Malformed input** — truncation, impossible item counts, size/offset contradictions and
  the `nr × nr2 × size` integer overflow are all detected rather than trusted

Support is tracked on four separate dimensions. “43 activities” describes the registry,
not verification of every historical revision.

| Dimension | Current coverage |
|---|---|
| Decode | Layout definitions for all 43 activities; unknown revisions are skipped with diagnostics |
| Meaning | Counter/gauge/identity and units in each activity's column metadata |
| Derived metrics | Shared rates, percentages and group calculations; unavailable values retain a reason |
| Output verification | Upstream corpus and regression tests cover selected generations, ABIs and activities; golden sadf cases cover FAN/IN/TEMP, not all 43 |

See [the activity specification](docs/format/02-activities.md) for revision details.

## Why

`sar` writes its logs (`/var/log/sa/saXX`) as raw C structs. That makes them fast to write
and painful to read: you generally need a `sysstat` install of a compatible vintage on a
compatible architecture. For example, your laptop's `sar` may not be able to open an old
log from a 32-bit PowerPC host.

reSARch reads supported formats directly, resolves struct layouts for the producer's ABI,
and reads logs collected on Linux from macOS and Windows too.

## How the output is verified

Every expected-output file that `sysstat` keeps in its own test suite is compared
**line by line** against what reSARch produces — 21 cases in total:

| Comparison result | Cases |
|---|---:|
| Byte-identical | 16 |
| Identical after masking | 4 |
| Mismatched | 0 |
| Not comparable | 1 |

The four masked cases mask exactly one thing: the `A_DISK` device-name column.
Upstream resolves `major:minor` through the **reading host's** `/dev` and `/sys`, so
`sda1` is a property of the machine that produced the expected output, not of the file.
reSARch deliberately prints `dev8-1` instead rather than inventing a name that would be
wrong for a log collected elsewhere.

SVG (`sadf -g`) uses reSARch's own drawing with the shared computed values. Its decoration
and coordinates are outside the byte comparison contract, so one SVG golden remains
explicitly excluded. Separate tests check chart values, selections, XML escaping, and
line breaks at missing samples and restarts.

Mismatches are never tolerated: a single one fails the suite. "Hard to implement" and
"the number doesn't match" are not accepted reasons to mask something.

### Reading verification results

Each status names what was checked; a generic “verified” would hide these distinctions.

| Status | What was checked | What it does not establish |
|---|---|---|
| Scan to EOF (`exact=true`, or `scan_exact=true` in measurement tables) | No early stop or incomplete record; the final scan offset equals the file size | Decoding every field, or matching computed values and text |
| All activities decoded (`activity_decode=complete`) | Every declared activity was planned and every sample decoded with no skipped activities | Availability of absent fields or unknown units, or matching historical `sar` output |
| Native reread matched | The same version of `sar` reread the saved `sa` and reproduced the collected text | Equality between reSARch and native output |
| Byte-identical / identical after masking | reSARch output matched native output in full or after excluding explicitly named columns | Equality for untested versions or activities |

### Verify all available official sysstat releases

```bash
make sar-upstream-all
# Replace the placeholder with the directory of the run to resume
SAR_MATRIX_OUTPUT="target/sar-matrix/<run-directory>" SAR_MATRIX_RESUME=1 make sar-upstream-all
```

The [pinned manifest](tools/sar-matrix/upstream.tsv) contains 181 cases: 133 official Git
versions and 48 recovered archive releases. Each version gets an image, native collection,
same-version `sar` rendering, and reSARch EOF scanning and rendering. Source commits and
SHA-256 hashes are pinned. The run retains binaries, native text, provenance, hashes, diffs,
and per-case logs. Failures do not prevent other cases from running; any failure makes the
final exit status nonzero. Results are written to `SUMMARY.tsv`.

On 2026-09-24, **all 181 cases passed collection, native rereading, and reSARch EOF scanning
and rendering**. The [measurement manifest](docs/measurements/upstream-matrix-2026-09-24.tsv)
and [sa/text pairs with provenance and SHA-256](testdata/sysstat-live/2026-09-24-official-all/)
are tracked in Git. These cover 27 official-source formats; separately collected CentOS
`0x1170` samples complete the 28 registered formats. Run `cargo test --test official_snapshots`
to recheck scanning and decoding of the 181 stored files. All 181 also passed
`activity_decode=complete`. Text comparisons differ in every case; none is byte-identical
to its historical native `sar` output. See [verification statuses](#reading-verification-results)
for the distinction between scanning, decoding, and output comparison.

Run `python3 tools/sar-matrix/update-upstream.py` to add new official tags.
**The manifest covers recovered and pinned releases, not every release ever published.**
Unavailable historical sources are not counted as successful tests.
See [the runbook](docs/format/06-live-matrix.md) for prerequisites and artifact details.

### Live sysstat collection matrix (2026-09-24)

In addition to the upstream golden suite, `sadc -S XALL` was run inside Apple Container
against five distribution packages and four pinned upstream releases. Each case recorded
two samples on Linux 6.18.35 / aarch64 / LP64, then reSARch followed record boundaries
through the end of the file.

In both tables below, `exact=true` means [a successful scan to EOF](#reading-verification-results).
Decoding coverage and native output comparisons are described separately.

| Source | sysstat | Format magic | reSARch scan to EOF |
|---|---:|---:|:---:|
| Alpine 3.23 | 12.7.8 | `0x2175` | `exact=true` |
| Debian 13 | 12.7.5 | `0x2175` | `exact=true` |
| Ubuntu 24.04 LTS | 12.6.1 | `0x2175` | `exact=true` |
| Fedora 44 | 12.7.9 | `0x2175` | `exact=true` |
| Rocky Linux 9 | 12.5.4 | `0x2175` | `exact=true` |
| Upstream | 10.2.1 | `0x2171` | `exact=true` |
| Upstream | 11.6.6 | `0x2173` | `exact=true` |
| Upstream | 12.0.6 | `0x2175` | `exact=true` |
| Upstream (latest at collection time) | 12.8.0 | `0x2175` | `exact=true` |

For upstream 12.8.0, all displayed values matched. The only text difference was the disk
name: upstream resolved `254:0` / `254:16` as `vda` / `vdb`, while reSARch deliberately
kept the portable names `dev254-0` / `dev254-16`. Older versions use their own period's
columns, so their text is retained as a diagnostic diff rather than compared against the
current 12.8.0 rendering profile.

The tracked [measurement manifest](docs/measurements/sar-matrix-2026-09-24.tsv) records
the source image or pinned commit, ABI, record count, exact-EOF result, and SHA-256 of both
files in every pair. The matching `sa` binary and upstream `sar -A -C -t` text are checked
in under [testdata/sysstat-live/2026-09-24](testdata/sysstat-live/2026-09-24). They contain
two samples from the disposable container VM and use the synthetic hostname
`resarch-fixture`; they contain no production-host data. See the
[snapshot notice](testdata/sysstat-live/README.md) for scope and provenance.

```bash
make sar-latest       # latest pinned upstream release
make sar-matrix       # distribution packages
make sar-generations  # format generations 0x2171, 0x2173 and 0x2175
```

See [the live-matrix runbook](docs/format/06-live-matrix.md) for artifact details and the
Docker-compatible fallback.

### CentOS Vault measurement pairs (2026-09-24)

Official RPMs for eleven CentOS releases produced `sa` / matching-version `sar` text
pairs, tracked under [testdata/sysstat-live/2026-09-24](testdata/sysstat-live/2026-09-24).
Repeated reads with the native `sar` reproduced the stored text in all eleven cases.

| Target CentOS RPM release | sysstat RPM version | Format magic | reSARch scan to EOF |
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

Collection used x86_64 / LP64 via Apple Container and Rosetta on Linux 6.18.35.
The 3.9 / 4.9 RPMs ran on a 5.11 rootfs, 6.0 / 6.5 on 6.6, and 8.0 / 8.5 on 8.4.2105.
Case names identify the target RPM release, not a recreation of the complete historical OS
or kernel. Reading and scans to EOF are verified with reSARch for all eleven cases,
including CentOS 3.9/4.9/5.11. See [legacy format coverage](#centos-345-and-earlier-formats)
for their per-CPU IRQ and other limitations, and differences from historical `sar` output.
Each pair's `PROVENANCE.json` records RPM URLs and SHA-256, OCI digests, environment and
commands; the [measurement table](docs/measurements/centos-matrix-2026-09-24.tsv) records
pair hashes and verification states.

```bash
make sar-all                         # Fetch, collect and verify all 20 recorded cases
make sar-centos                      # Only the 11 CentOS cases
scripts/collect-sar-matrix.sh centos-6.5  # Collect one case again
```

Each run creates `target/sar-matrix/<UTC timestamp>/`, containing each case's `sa`,
`sar-A.txt`, provenance, SHA-256 and log, plus the combined `SUMMARY.tsv`.
Failed cases are recorded, remaining cases continue, and the overall run exits nonzero.
See the [runbook](docs/format/06-live-matrix.md) for prerequisites, collection settings
and rootfs/RPM mappings.

## What "sar compatible" means here

The phrase covers three separate things, and reSARch states each one explicitly rather
than implying all of them:

| Axis | Scope |
|---|---|
| **Input compatibility** | The 28 [supported formats](#supported-formats), with legacy field and ABI limitations |
| **Computation and output** | Which `sar` / `sadf` version's rendering is reproduced |
| **CLI compatibility** | Which options and calling conventions are accepted |

The `resarch` binary does not perform live sampling (`sadc`). The bundled collection
script runs native `sadc` inside each image. Binary output from `resarch` is limited
to re-encoding a file it just read (`sadf -c`), leaving every value alone.
Text and charts can be saved with `sa2sar`, `detect --svg-dir`, and the other output commands.
The generation of the file being read and the output format being reproduced are separate
settings. By default every supported format uses the `sysstat` 12.8.0 rendering profile;
`--sar-profile` selects another `sar` to reproduce instead (see below).

Options that are parsed but not yet acted upon are rejected at run time with a reason:

| Option | Status |
|---|---|
| `sar -o` / `--sadc` | Collection is out of scope — rejected explicitly |
| `sadf -l` | PCP output is not implemented |
| `sadf -g -O autoscale,packed,customcol` | These SVG options are explicitly rejected; skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday are supported |
| `sadf -H` combined with another format | Not implemented — use `resarch sadf -H <file>` |

### Reproducing RHEL / CentOS 7's `sar`

```bash
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13
resarch --sar-profile sysstat-10.1.5-el7 -A -f sa13        # the sar entry point, el7 grammar
resarch --sar-profile sysstat-10.1.5-el7 -R -f sa13 --sar-page-size 65536   # a ppc64le host
```

| `--sar-profile` | Reproduces | Reads |
|---|---|---|
| `current` (= `sysstat-12.8.0`, default) | upstream `sysstat` 12.8.0 | every supported input format |
| `sysstat-10.1.5-el7` | RHEL / CentOS 7's `sysstat-10.1.5-17.el7` … `-20.el7_9` | `format_magic` 0x2171 only, as 10.1.5 itself |

A report written by a RHEL 7 host differs from the default rendering in more than column
headers, so the profile reproduces the whole of that `sar`:

- **Columns**: `-B` ends with `%vmeff`, `-d` shows `rd_sec/s … svctm`, `-A` includes `-R`
  (`frmpg/s bufpg/s campg/s`), `-r` has ten columns, `-n DEV` has no `%ifutil`
- **Arithmetic**: the CPU `all` row divides the file's aggregate slot by the record
  header's uptime instead of re-summing the CPUs; offline CPUs print `0.00` and carry their
  last values forward; some averages divide integers before converting to floating point
  (a `kbswpcad` average of 15.5 prints as `15`, not `16`)
- **Layout**: `LINUX RESTART` lines carry no CPU count, `COM` lines keep their text as is,
  and the header repeats every 11 samples under `-A` because Red Hat raised `NR_CPUS` to 8192
- **Grammar** (on the `sar` entry point): `-h` is help, `-R` exists, `-I` takes `XALL` and
  interrupt numbers; options that 10.1.5 does not have are usage errors

The profile is never picked automatically. A file's header records the `sadc` that wrote
it, not the `sar` that `sa2` ran or the patches it was built with, and switching the output
on a guess would change reports behind your back. Two inputs cannot come from the file at
all: `-R` converts kilobytes to pages with the page size of the host running `sar`
(`--sar-page-size`, default 4096 — the x86_64 value), and `-p` / `-j` look up device names
on that host, which reSARch does not do for a log collected elsewhere (`dev<major>-<minor>`
stays). `sadf` has no profile and refuses
the option.

This was checked against a RHEL 7 host's own reports — 29 days of `sa` binaries and the
`sarDD` its `sa2` wrote match byte for byte — and against el7's `sar` built from the CentOS
source RPM, on real files and on synthetic ones covering every activity and the corner
cases (offline CPUs, counter wrap, interface and disk re-registration, restarts, comments).
The rules and the verification procedure are in
[`docs/format/05-sysstat-10.1.5-el7.md`](docs/format/05-sysstat-10.1.5-el7.md).

### Environment variables `sar` reads

Upstream `sar` takes part of its formatting from the environment, so reSARch reads the
same variables for its compatible output (`sar`, `sa2sar`, `show --format sar`).

| Variable | Accepted value | Effect |
|---|---|---|
| `S_TIME_FORMAT` | exactly `ISO` | Banner date becomes `YYYY-MM-DD` instead of `MM/DD/YY` |
| `S_REPEAT_HEADER` | all digits, `> 0` | Reprints column headers every N lines — **only when stdout is not a terminal** |

Matching is as strict as upstream's: `S_TIME_FORMAT=iso` does nothing, and a
`S_REPEAT_HEADER` with a sign, spaces, or non-digits is ignored rather than rejected.

When stdout *is* a terminal, the header interval comes from the window height
(`rows - 2`), and `S_REPEAT_HEADER` is not consulted — the same `else if` that upstream
has. With neither source available the interval is 86400 lines.

That interval is not as large as it looks. Upstream counts a sample as
`count_bits(cpu_bitmap)` lines for the activities that use the CPU bitmap, and `-A` and
`-P ALL` fill the whole bitmap, so each sample counts as 8200 lines regardless of how
many CPUs the host has. `sar -A` therefore reprints its CPU header every 11 samples even
in a pipe. reSARch reproduces this.

These variables do not affect `sadf`: upstream never sets `S_F_PREFD_TIME_OUTPUT` there,
so its dates stay `%Y-%m-%d` and its timestamps stay `%H:%M:%S` regardless.
Under `--sar-profile sysstat-10.1.5-el7`, `S_REPEAT_HEADER` is not read either — that
variable arrived after 10.1.5 — while `S_TIME_FORMAT` works as described.

## Design notes

Things that turned out to matter, documented in [`docs/design.md`](docs/design.md):

- In the statistical payload of self-describing formats, `unsigned long` occupies an
  **8-byte slot**; on 32-bit producers only the leading 4 bytes hold the value. Older formats
  use generation-specific widths and layouts ([2.2](docs/format/08-packed-legacy.md),
  [3.2.4–8.1.2](docs/format/09-legacy-generations.md)).
- An item's stride is **always the declared `file_activity.size`**, never a size computed
  from the struct definition — older `A_HUGE` records disagree with their own layout.
- Two `0x2175` variants report an identical `header_size` of 328 while differing inside;
  only the per-type field counts distinguish them.
- Missing and zero are different. A field the producing version never wrote is
  `UnsupportedBySource`, not `0`, so averages and threshold checks cannot silently drift.

Full format specifications live in [`docs/format/`](docs/format/).

## Development

Requires [mise](https://mise.jdx.dev/). Tool versions are pinned in `mise.toml`. Every
`make` target runs its tools through `mise exec`, so mise does not have to be activated in
your shell. Without mise, add `SYSTEM_TOOLS=1` to use the tools on your PATH; their
versions may then differ from CI.

```bash
make setup   # install the toolchain (mise install) and fetch dependencies
make ci      # the same checks as the CI Test job
```

Run `make` with no arguments to list the targets:

| Command | Description |
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

`make conformance` downloads the upstream data with `make fixtures` and then runs the
suites that `make test` skips. It needs `xmllint`, which macOS ships and Debian / Ubuntu
provide in `libxml2-utils`. The test that records the version of sysstat's own `sar`
skips itself when `sar` is not installed; CI installs sysstat on Ubuntu 24.04 so that the
baseline it compares against stays fixed and is recorded in the job summary.

The `sar-*` targets collect and verify real `sa` files inside containers (see
[How the output is verified](#how-the-output-is-verified)). They need Apple container or
Docker, plus `jq`, `curl`, and `shasum` on the host.

Upstream `sysstat` is GPL-licensed, so **none of its test data or expected output is
vendored here**. `make fixtures` fetches it into `target/fixtures/` at a pinned tag with
SHA-256 verification, for the conformance suite only. The data in `tests/fixtures/` is
written from scratch and MIT-licensed like the rest of the project. Live snapshots in
`testdata/sysstat-live/` are separate; see their [provenance and usage notice](testdata/sysstat-live/README.md).

reSARch is an independent implementation written from the on-disk format, not a port of
`sysstat` source.

See [CHANGELOG.md](CHANGELOG.md) for release notes. Report bugs through
[GitHub Issues](https://github.com/owayo/re-sar-ch/issues), including `resarch --version`,
the command you ran, and the error message.

## Release

Releases are made from GitHub Actions: **Actions > Release > Run workflow**. The workflow
bumps the version in `Cargo.toml` and `Cargo.lock`, tags the commit, builds the six
binaries listed under [From GitHub Releases](#from-github-releases), and publishes the
GitHub Release. The same workflow then updates the Homebrew tap and submits the winget
manifest.

- Turn on **dry_run** to compute the next version only. Nothing is committed, tagged,
  built, or published.
- Versions use CalVer, `YY.M.PATCH` (for example `26.9.106`), where `PATCH` starts at 100
  each month. The number does not show whether a release breaks compatibility, so read the
  "⚠️ 破壊的変更" (breaking changes) entries that open each version in
  [CHANGELOG.md](CHANGELOG.md) before upgrading.

## License

MIT — see [LICENSE](LICENSE).
