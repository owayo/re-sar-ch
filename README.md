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

A Rust toolchain is required.

```bash
cargo install --git https://github.com/owayo/re-sar-ch
```

To build from a local checkout:

```bash
git clone https://github.com/owayo/re-sar-ch.git
cd re-sar-ch
make install
```

`make install` installs the binary and the Claude / Codex skills. Use `make install-bin` for the binary only. On Windows, use `cargo install --path .`.

### From GitHub Releases

Once published, platform binaries will be available from [Releases](https://github.com/owayo/re-sar-ch/releases).

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

> `winget install owayo.reSARch` does this for you (it registers `resarch` on PATH), so the manual download is only needed if you do not use winget. After a winget install, open a new terminal so the updated PATH takes effect.

## Usage

### As a drop-in for `sar`

reSARch accepts `sar`'s own argument syntax. Omit the subcommand and it behaves like `sar`:

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
```

The quirks are reproduced deliberately: `-I` takes no number, `-P ALL` differs from
`-P all`, `-h` means `--pretty --human` rather than help, and the first record matched by
`-s` is consumed as the baseline rather than displayed.

### Saving an sa binary as sar text

```bash
resarch sa2sar sa13 -o sar13             # All activities, averages, restarts and comments
resarch sa2sar sa13 --utc -o sar13-utc   # Use UTC timestamps
resarch sa2sar sa13                     # Write to stdout for piping
```

The default is equivalent to `sar -A -C -t -f sa13`, using timestamps recorded by
the source host. Omit `-o` or use `-o -` for stdout. Existing destination files
are never overwritten; failed conversion leaves no partial destination file.
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

Every key is lower case or a symbol. When `Shift` selects a different action, a slip of
the finger runs the wrong feature instead of doing nothing.

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

Activities with many columns (MEMORY has 19) cannot all fit: laid out side by side they
squash to a few digits each and none of them stay readable. The table narrows them two ways.

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

It follows the same rules as every other output:

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

Every format generation, verified byte-for-byte against upstream's own test corpus:

| `format_magic` | sysstat versions | notes |
|---|---|---|
| `0x2170` | … 9.1.5 | oldest upstream generation reSARch can read; upstream itself cannot convert these |
| `0x1170` | 9.0.4 (RHEL/CentOS 6.5+) | **vendor variant** — Red Hat renumbered the magic to a value no upstream release uses; `stats_io` is 80 bytes instead of 20 |
| `0x2171` | 9.1.6 … 10.2 | 8-byte file magic, no RESTART payload |
| `0x2173` | 10.3 … 11.6 | RESTART records carry a volatile-activity list that changes item counts |
| `0x2175` | 11.7 … 12.8 | self-describing layout, `extra_desc` chains, three internal variants |

`0x1170` does not exist in any upstream release. Red Hat changed the on-disk layout of
`stats_io` in RHEL 6.3 without bumping the format version, which made the new `sar`
silently misread older files; the fix in RHEL 6.5 renumbered the magic instead. Since no
upstream build ever emits a 20-byte-vs-80-byte ambiguity, the declared item size alone
identifies the variant.

### Older generations — identified, not yet readable

sysstat 3.2.4 through 8.1.2 (`0x115a` … `0x216f`) use a fundamentally different layout:
no `file_activity[]` array, no `record_header`, and — up to `0x216e` — no `file_magic` at
the head of the file either. In those generations the magic lives *inside* `file_hdr`, and
**its offset moves across generations** (4 → 36 → 32), so "read the first two bytes" does
not identify them at all.

`0x216f` (8.1.1 / 8.1.2) is a **transitional** generation: it gained `file_magic` at the head
of the file while the body was still the old layout. The switch to `file_activity[]` only
happens in `0x2170`.

reSARch identifies them and reports which sysstat wrote the file:

```
sa07: sysstat 6.1.3〜7.0.4 が書いた形式です (format_magic=0x2169)。この世代の読み取りは未実装です
```

That is deliberately distinct from "not a sysstat file" and from "unsupported format" —
confusing the three sends you down the wrong debugging path. Decoding these generations
is not implemented yet.

Also handled:

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
compatible architecture. A five-year-old log from a 32-bit PowerPC box is not something
your laptop's `sar` will open.

reSARch reads the bytes directly. It knows every on-disk format generation `sysstat` has
shipped, resolves struct layouts from the producer's ABI rather than the host's, and runs
anywhere Rust runs — including macOS and Windows, on logs collected from Linux.

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

## What "sar compatible" means here

The phrase covers three separate things, and reSARch states each one explicitly rather
than implying all of them:

| Axis | Scope |
|---|---|
| **Input compatibility** | Which format generations can be read — all four |
| **Computation and output** | Which `sar` / `sadf` version's rendering is reproduced |
| **CLI compatibility** | Which options and calling conventions are accepted |

reSARch never collects: live sampling (`sadc`) is out of scope. Binary output is limited
to re-encoding a file it just read (`sadf -c`), leaving every value alone.
Text and charts can be saved with `sa2sar`, `detect --svg-dir`, and the other output commands.
The generation of the file being read and the output format being reproduced are separate
settings: a v10 file can be rendered in v12 `sar` style, and vice versa.

Options that are parsed but not yet acted upon are rejected at run time with a reason:

| Option | Status |
|---|---|
| `sar -o` / `--sadc` | Collection is out of scope — rejected explicitly |
| `sadf -l` | PCP output is not implemented |
| `sadf -g -O autoscale,packed,customcol` | These SVG options are explicitly rejected; skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday are supported |
| `sadf -H` combined with another format | Not implemented — use `resarch sadf -H <file>` |

## Design notes

Things that turned out to matter, documented in [`docs/design.md`](docs/design.md):

- `unsigned long` always occupies an **8-byte slot** on disk; on 32-bit producers only the
  leading 4 bytes are meaningful. This is why struct sizes agree across word sizes.
- An item's stride is **always the declared `file_activity.size`**, never a size computed
  from the struct definition — older `A_HUGE` records disagree with their own layout.
- Two `0x2175` variants report an identical `header_size` of 328 while differing inside;
  only the per-type field counts distinguish them.
- Missing and zero are different. A field the producing version never wrote is
  `UnsupportedBySource`, not `0`, so averages and threshold checks cannot silently drift.

Full format specifications live in [`docs/format/`](docs/format/).

## Development

```bash
make build        # debug build
make test         # unit and integration tests
make check        # clippy + fmt
make fixtures     # fetch upstream test data (see below)
make release      # optimised build
make install      # install the binary and the agent skill (claude + codex)
make install-bin  # binary only
make uninstall    # remove both
```

Upstream `sysstat` is GPL-licensed, so **none of its test data or expected output is
vendored here**. `make fixtures` fetches it into `target/fixtures/` at a pinned tag with
SHA-256 verification, for the conformance suite only. The fixtures committed to this
repository are written from scratch and are MIT-licensed like the rest of the project.

reSARch is an independent implementation written from the on-disk format, not a port of
`sysstat` source.

## License

MIT — see [LICENSE](LICENSE).
