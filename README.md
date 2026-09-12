<h1 align="center">reSARch</h1>

<p align="center">
  A standalone parser for <code>sysstat</code> binary log files (<code>sa</code> files).<br>
  No <code>sar</code>, no <code>sadf</code>, no C library — one binary reads them all.
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

## Why

`sar` writes its logs (`/var/log/sa/saXX`) as raw C structs. That makes them fast to write
and painful to read: you generally need a `sysstat` install of a compatible vintage on a
compatible architecture. A five-year-old log from a 32-bit PowerPC box is not something
your laptop's `sar` will open.

reSARch reads the bytes directly. It knows every on-disk format generation `sysstat` has
shipped, resolves struct layouts from the producer's ABI rather than the host's, and runs
anywhere Rust runs — including macOS and Windows, on logs collected from Linux.

## What it reads

Every format generation, verified byte-for-byte against upstream's own test corpus:

| `format_magic` | sysstat versions | notes |
|---|---|---|
| `0x2170` | … 9.1.5 | oldest known; upstream itself cannot convert these |
| `0x2171` | 9.1.6 … 10.2 | 8-byte file magic, no RESTART payload |
| `0x2173` | 10.3 … 11.6 | RESTART records carry a volatile-activity list that changes item counts |
| `0x2175` | 11.7 … 12.8 | self-describing layout, `extra_desc` chains, three internal variants |

Also handled:

- **All 43 activities** — CPU, memory, disk, every IPv4/IPv6 protocol, PSI, power sensors,
  filesystems, HugePages, interrupts, and the rest
- **Big-endian and 32-bit producers** — a PowerPC log opens the same as an x86-64 one
- **Files converted by `sadf -c`** — the `upgraded` marker is read and reported, and
  reSARch can perform the conversion itself
- **Malformed input** — truncation, impossible item counts, size/offset contradictions and
  the `nr × nr2 × size` integer overflow are all detected rather than trusted

## Install

Grab a binary from [Releases](https://github.com/owayo/re-sar-ch/releases), or build it:

```bash
cargo install --git https://github.com/owayo/re-sar-ch
```

```bash
# macOS (Apple Silicon)
curl -L https://github.com/owayo/re-sar-ch/releases/latest/download/resarch-aarch64-apple-darwin.tar.gz | tar xz
sudo mv resarch /usr/local/bin/
```

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
```

The quirks are reproduced deliberately: `-I` takes no number, `-P ALL` differs from
`-P all`, `-h` means `--pretty --human` rather than help, and the first record matched by
`-s` is consumed as the baseline rather than displayed.

### Its own commands

```bash
resarch info sa01                        # generation, ABI, activity table
resarch show sa01 --activity cpu,disk --format table
resarch show sa01 --format ndjson        # for feeding an agent or a pipeline
resarch detect sa01                      # where and what looks off
resarch summarize sa01 sa02 --format json
resarch summarize sa01 sa02 sa03 --from 09:00 --to 18:00  # only 9am-6pm of each day
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

For `summarize` and `compare`, `--from` / `--to` narrow **the aggregation period itself**:
samples outside the range enter neither the mean, the p95, nor the delta totals, and the
period bounds shrink with them. `detect` reads the same two options differently — they
narrow what gets reported, not the material its comparison basis is built from.

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

### Converting an old file

```bash
resarch sadf -c sa01 > sa01-current        # 0x2171 / 0x2173 → 0x2175
resarch sadf -c sa01 -O hz=250 > out       # override the assumed HZ
```

Old headers do not record HZ, and upstream's `sadf -c` substitutes the HZ of whatever
machine runs the conversion — so the same input produces different output elsewhere.
reSARch estimates it from the file's own `uptime` counters instead, never pairing samples
across a restart, and reports which value it used and how it got there.

## How the output is verified

Every expected-output file that `sysstat` keeps in its own test suite is compared
**line by line** against what reSARch produces — 21 cases in total:

| | |
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

The one case that cannot be compared is `sadf -g`, which draws SVG. That output format
is not implemented, and the suite reports it as "not comparable" on every run rather than
quietly counting it as a pass.

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

reSARch never collects: live sampling (`sadc`) is out of scope. The only thing it writes
is a re-encoding of a file it just read (`sadf -c`), and that leaves every value alone.
The generation of the file being read and the output format being reproduced are separate
settings: a v10 file can be rendered in v12 `sar` style, and vice versa.

Options that are parsed but not yet acted upon are rejected at run time with a reason:

| Option | Status |
|---|---|
| `sar -o` / `--sadc` | Collection is out of scope — rejected explicitly |
| `sadf -g` / `-l` | SVG and PCP output are not implemented |
| `sadf --dev=` / `--iface=` / `--fs=` / `--int=` | Item-name filters reach `sar` but not `sadf` yet |
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
```

Upstream `sysstat` is GPL-licensed, so **none of its test data or expected output is
vendored here**. `make fixtures` fetches it into `target/fixtures/` at a pinned tag with
SHA-256 verification, for the conformance suite only. The fixtures committed to this
repository are written from scratch and are MIT-licensed like the rest of the project.

reSARch is an independent implementation written from the on-disk format, not a port of
`sysstat` source.

## License

MIT — see [LICENSE](LICENSE).
