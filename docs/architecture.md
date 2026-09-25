# Architecture

## Why

`sar` writes its logs (`/var/log/sa/saXX`) as raw C structs. That makes them fast to write and painful to read: you generally need a `sysstat` install of a compatible vintage on a compatible architecture. For example, your laptop's `sar` may not be able to open an old log from a 32-bit PowerPC host.

reSARch reads supported formats directly, resolves struct layouts for the producer's ABI, and reads logs collected on Linux from macOS and Windows too.

## Design notes

Things that turned out to matter, documented in [`docs/design.md`](design.md):

- In the statistical payload of self-describing formats, `unsigned long` occupies an **8-byte slot**; on 32-bit producers only the leading 4 bytes hold the value. Older formats use generation-specific widths and layouts ([2.2](format/08-packed-legacy.md), [3.2.4–8.1.2](format/09-legacy-generations.md)).
- An item's stride is **always the declared `file_activity.size`**, never a size computed from the struct definition — older `A_HUGE` records disagree with their own layout.
- Two `0x2175` variants report an identical `header_size` of 328 while differing inside; only the per-type field counts distinguish them.
- Missing and zero are different. A field the producing version never wrote is `UnsupportedBySource`, not `0`, so averages and threshold checks cannot silently drift.

Full format specifications live in [`docs/format/`](format/). The design notes and the specifications are written in Japanese.
