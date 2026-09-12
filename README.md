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
- **Files converted by `sadf -c`** — the `upgraded` marker is read and reported
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
resarch summarize sa01 sa02 --format json
resarch compare --host app1=app1/sa01 --host app2=app2/sa01
```

## What "sar compatible" means here

The phrase covers three separate things, and reSARch states each one explicitly rather
than implying all of them:

| Axis | Scope |
|---|---|
| **Input compatibility** | Which format generations can be read — all four |
| **Computation and output** | Which `sar` / `sadf` version's rendering is reproduced |
| **CLI compatibility** | Which options and calling conventions are accepted |

reSARch is a **reader**. Live collection (`sadc`) is out of scope.
The generation of the file being read and the output format being reproduced are separate
settings: a v10 file can be rendered in v12 `sar` style, and vice versa.

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
