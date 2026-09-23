# sysstat live snapshots

This directory contains deliberately selected pairs of files produced inside a disposable
Linux VM by the sysstat version named in the measurement manifest:

- `sa`: binary output from the release's own collector (`sar` for 2.2, otherwise
  `sadc`), using the collection options supported by that release.
- `sar-A.txt`: text from the same sysstat version with `LC_ALL=C TZ=UTC`.
  Rendering options also depend on the release; see the collection scripts and runbook.

The hostname is fixed to `resarch-fixture`. These are short compatibility snapshots (usually two records; sysstat 2.2 also stores an initial baseline),
not data copied from a production host. Each dated snapshot has its source image or pinned
upstream commit, ABI, format generation, exact-EOF result, and SHA-256 values recorded under
[`docs/measurements/`](../../docs/measurements/).

The CentOS pairs also include `PROVENANCE.json` with the official Vault RPM URLs and
SHA-256, the OCI image digests, actual rootfs release, kernel, commands and reader status.
CentOS case names identify the RPM's target release. Some run on a different rootfs:
3.9 / 4.9 on 5.11, 6.0 / 6.5 on 6.6, and 8.0 / 8.5 on 8.4.2105. All use the contemporary
container VM kernel, not the original distribution kernel. The oldest three pairs were
successfully read twice by their native `sar`. Their immutable `PROVENANCE.json` files
retain the original collection-time reader status (`resarch_scan_exact: null`). After adding
legacy decoding, all three were rechecked with exact EOF and decoded in the regression suite;
the measurement TSV records the updated verification result.

`make sar-all` collects all twenty recorded cases into a new untracked timestamp directory.
`make sar-centos` selects only the eleven CentOS cases.
`make sar-upstream-all` builds and verifies all 181 pinned official/archive release cases. See the
[collection runbook](../../docs/format/06-live-matrix.md) for the runtime requirements.

The [2026-09-24 official-release snapshot](2026-09-24-official-all/) contains all 181
pairs from that manifest. Each case passed native rereading, exact-EOF scanning, and reSARch
rendering, plus decoding of every declared activity with no skips. Its `PROVENANCE.json`
pins source provenance and verification results (`activity_decode=complete`);
`READER-SHA256SUMS` at the snapshot root identifies the reader binaries used for verification
(the binaries themselves are not included). The
[measurement table](../../docs/measurements/upstream-matrix-2026-09-24.tsv) records text
comparison separately: exact EOF does not imply historical native-text equivalence.
`cargo test --test official_snapshots` checks all 181 files without running containers.

These files are kept apart from `tests/fixtures/`: they are live outputs used for manual
cross-version verification and must not be used to derive the independently authored test
fixtures. Normal matrix runs remain untracked under `target/sar-matrix/`.

sysstat is distributed under GPL-2.0-or-later. No sysstat executable or source code is
included here; these generated compatibility artifacts are retained with their provenance
and are not a relicensing of sysstat by this project's MIT license. See the
[upstream sysstat license](https://github.com/sysstat/sysstat/blob/master/COPYING).
