# Supported formats

All 28 registered format magic values have readers. Legacy decoding limitations and the scope of scan and output verification are described below.

| `format_magic` | sysstat versions | notes |
|---|---|---|
| `0x015d` | 2.2 | Packed selected columns; [limitations and ABI](format/08-packed-legacy.md) |
| `0x115a`, `0x215a`…`0x216f` (22 formats, excluding unused `0x215c`) | 3.2.4 … 8.1.2 | Monolithic formats, including CentOS 3/4/5 and `0x2168` from 6.1.1–6.1.2; [generation evidence](format/09-legacy-generations.md) |
| `0x2170` | 8.1.3 … 9.1.5 | Introduced activity arrays; upstream itself cannot convert these |
| `0x1170` | 9.0.4 (RHEL/CentOS 6.5+) | **vendor variant** — Red Hat renumbered the magic to a value no upstream release uses; `stats_io` is 80 bytes instead of 20 |
| `0x2171` | 9.1.6 … 10.2 | 8-byte file magic, no RESTART payload |
| `0x2173` | 10.3 … 11.6 | RESTART records carry a volatile-activity list that changes item counts |
| `0x2175` | 11.7 … 12.8 | self-describing layout, `extra_desc` chains, three internal variants |

`0x1170` does not exist in any upstream release. Red Hat changed the on-disk layout of `stats_io` in RHEL 6.3 without bumping the format version, which made the new `sar` silently misread older files; the fix in RHEL 6.5 renumbered the magic instead. Since no upstream build ever emits a 20-byte-vs-80-byte ambiguity, the declared item size alone identifies the variant.

## CentOS 3/4/5 and earlier formats

sysstat 3.2.4 through 8.1.2 (`0x115a` … `0x216f`) use a fundamentally different layout: no `file_activity[]` array, no `record_header`, and — up to `0x216e` — no `file_magic` at the head of the file either. In those generations the magic lives *inside* `file_hdr`, and **its offset moves across generations** (4 → 36 → 32), so "read the first two bytes" does not identify them at all.

`0x216f` (8.1.1 / 8.1.2) is a **transitional** generation: it gained `file_magic` at the head of the file while the body was still the old layout. The switch to `file_activity[]` only happens in `0x2170`.

All 22 monolithic formats can be read directly, including `0x2168` from sysstat 6.1.1–6.1.2. Layouts were measured from original sources. Native samples from 24 releases pass `exact=true` scans and rendering; independent fixtures cover both endiannesses, 32/64-bit long widths, array boundaries, and truncation.

The reader exposes supported fields present in each generation. Old per-CPU IRQ arrays are bounded and skipped; PID-bearing pipe streams are rejected. Reports use the current column layout and calculation rules: **successful scanning does not imply byte-identical historical `sar` output**. Unrecorded fields and values whose units cannot be established remain unavailable.

Formats through `0x2167` do not record the long width; the default assumption is 8 bytes, with a diagnostic. Library callers can set `OpenOptions.legacy_long_bytes = 4`. The CLI currently has no override; use the library for 32-bit inputs that omit this width. `0x115a` has no epoch or timezone, so its date and clock values are interpreted as UTC. Formats through `0x216e` do not record the precise sysstat version or machine architecture. See [legacy format specifications and verification](format/09-legacy-generations.md).

sysstat 2.2 (`0x015d`) has a separate packed-column reader. Its payload endianness is not recorded; the default is little-endian with a diagnostic, overridable through `OpenOptions.legacy_endian`. Releases whose sources have not been recovered, such as 1.x and 3.0–3.1, are not assumed to share a neighboring layout. **Supporting all registered formats is distinct from having verified every historical release.**

## Activity and ABI coverage

The producer ABI determines details such as integer widths and byte order. The following are also supported, subject to the legacy limitations above:

- **All 43 activities** — CPU, memory, disk, every IPv4/IPv6 protocol, PSI, power sensors, filesystems, HugePages, interrupts, and the rest
- **Big-endian and 32-bit producers** — a PowerPC log opens the same as an x86-64 one
- **Files converted by `sadf -c`** — the `upgraded` marker is read and reported, and reSARch can perform the conversion itself
- **Malformed input** — truncation, impossible item counts, size/offset contradictions and the `nr × nr2 × size` integer overflow are all detected rather than trusted

Support is tracked on four separate dimensions. “43 activities” describes the registry, not verification of every historical revision.

| Dimension | Current coverage |
|---|---|
| Decode | Layout definitions for all 43 activities; unknown revisions are skipped with diagnostics |
| Meaning | Counter/gauge/identity and units in each activity's column metadata |
| Derived metrics | Shared rates, percentages and group calculations; unavailable values retain a reason |
| Output verification | Upstream corpus and regression tests cover selected generations, ABIs and activities; golden sadf cases cover FAN/IN/TEMP, not all 43 |

See [the activity specification](format/02-activities.md) for revision details. The specifications under [`format/`](format/) are written in Japanese.
