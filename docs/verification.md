# How the output is verified

Every expected-output file that `sysstat` keeps in its own test suite is compared **line by line** against what reSARch produces — 21 cases in total:

| Comparison result | Cases |
|---|---:|
| Byte-identical | 16 |
| Identical after masking | 4 |
| Mismatched | 0 |
| Not comparable | 1 |

The four masked cases mask exactly one thing: the `A_DISK` device-name column. Upstream resolves `major:minor` through the **reading host's** `/dev` and `/sys`, so `sda1` is a property of the machine that produced the expected output, not of the file. reSARch deliberately prints `dev8-1` instead rather than inventing a name that would be wrong for a log collected elsewhere.

SVG (`sadf -g`) uses reSARch's own drawing with the shared computed values. Its decoration and coordinates are outside the byte comparison contract, so one SVG golden remains explicitly excluded. Separate tests check chart values, selections, XML escaping, and line breaks at missing samples and restarts.

Mismatches are never tolerated: a single one fails the suite. "Hard to implement" and "the number doesn't match" are not accepted reasons to mask something.

## Reading verification results

Each status names what was checked; a generic “verified” would hide these distinctions.

| Status | What was checked | What it does not establish |
|---|---|---|
| Scan to EOF (`exact=true`, or `scan_exact=true` in measurement tables) | No early stop or incomplete record; the final scan offset equals the file size | Decoding every field, or matching computed values and text |
| All activities decoded (`activity_decode=complete`) | Every declared activity was planned and every sample decoded with no skipped activities | Availability of absent fields or unknown units, or matching historical `sar` output |
| Native reread matched | The same version of `sar` reread the saved `sa` and reproduced the collected text | Equality between reSARch and native output |
| Byte-identical / identical after masking | reSARch output matched native output in full or after excluding explicitly named columns | Equality for untested versions or activities |

## Verify all available official sysstat releases

```bash
make sar-upstream-all
# Replace the placeholder with the directory of the run to resume
SAR_MATRIX_OUTPUT="target/sar-matrix/<run-directory>" SAR_MATRIX_RESUME=1 make sar-upstream-all
```

The [pinned manifest](../tools/sar-matrix/upstream.tsv) contains 181 cases: 133 official Git versions and 48 recovered archive releases. Each version gets an image, native collection, same-version `sar` rendering, and reSARch EOF scanning and rendering. Source commits and SHA-256 hashes are pinned. The run retains binaries, native text, provenance, hashes, diffs, and per-case logs. Failures do not prevent other cases from running; any failure makes the final exit status nonzero. Results are written to `SUMMARY.tsv`.

On 2026-09-24, **all 181 cases passed collection, native rereading, and reSARch EOF scanning and rendering**. The [measurement manifest](measurements/upstream-matrix-2026-09-24.tsv) and [sa/text pairs with provenance and SHA-256](../testdata/sysstat-live/2026-09-24-official-all/) are tracked in Git. These cover 27 official-source formats; separately collected CentOS `0x1170` samples complete the 28 registered formats. Run `cargo test --test official_snapshots` to recheck scanning and decoding of the 181 stored files. All 181 also passed `activity_decode=complete`. Text comparisons differ in every case; none is byte-identical to its historical native `sar` output. See [verification statuses](#reading-verification-results) for the distinction between scanning, decoding, and output comparison.

Run `python3 tools/sar-matrix/update-upstream.py` to add new official tags. **The manifest covers recovered and pinned releases, not every release ever published.** Unavailable historical sources are not counted as successful tests. See [the runbook](format/06-live-matrix.md) for prerequisites and artifact details.

## Live sysstat collection matrix (2026-09-24)

In addition to the upstream golden suite, `sadc -S XALL` was run inside Apple Container against five distribution packages and four pinned upstream releases. Each case recorded two samples on Linux 6.18.35 / aarch64 / LP64, then reSARch followed record boundaries through the end of the file.

In both tables below, `exact=true` means [a successful scan to EOF](#reading-verification-results). Decoding coverage and native output comparisons are described separately.

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

For upstream 12.8.0, all displayed values matched. The only text difference was the disk name: upstream resolved `254:0` / `254:16` as `vda` / `vdb`, while reSARch deliberately kept the portable names `dev254-0` / `dev254-16`. Older versions use their own period's columns, so their text is retained as a diagnostic diff rather than compared against the current 12.8.0 rendering profile.

The tracked [measurement manifest](measurements/sar-matrix-2026-09-24.tsv) records the source image or pinned commit, ABI, record count, exact-EOF result, and SHA-256 of both files in every pair. The matching `sa` binary and upstream `sar -A -C -t` text are checked in under [testdata/sysstat-live/2026-09-24](../testdata/sysstat-live/2026-09-24). They contain two samples from the disposable container VM and use the synthetic hostname `resarch-fixture`; they contain no production-host data. See the [snapshot notice](../testdata/sysstat-live/README.md) for scope and provenance.

```bash
make sar-latest       # latest pinned upstream release
make sar-matrix       # distribution packages
make sar-generations  # format generations 0x2171, 0x2173 and 0x2175
```

See [the live-matrix runbook](format/06-live-matrix.md) for artifact details and the Docker-compatible fallback.

## CentOS Vault measurement pairs (2026-09-24)

Official RPMs for eleven CentOS releases produced `sa` / matching-version `sar` text pairs, tracked under [testdata/sysstat-live/2026-09-24](../testdata/sysstat-live/2026-09-24). Repeated reads with the native `sar` reproduced the stored text in all eleven cases.

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

Collection used x86_64 / LP64 via Apple Container and Rosetta on Linux 6.18.35. The 3.9 / 4.9 RPMs ran on a 5.11 rootfs, 6.0 / 6.5 on 6.6, and 8.0 / 8.5 on 8.4.2105. Case names identify the target RPM release, not a recreation of the complete historical OS or kernel. Reading and scans to EOF are verified with reSARch for all eleven cases, including CentOS 3.9/4.9/5.11. See [legacy format coverage](formats.md#centos-345-and-earlier-formats) for their per-CPU IRQ and other limitations, and differences from historical `sar` output. Each pair's `PROVENANCE.json` records RPM URLs and SHA-256, OCI digests, environment and commands; the [measurement table](measurements/centos-matrix-2026-09-24.tsv) records pair hashes and verification states.

```bash
make sar-all                         # Fetch, collect and verify all 20 recorded cases
make sar-centos                      # Only the 11 CentOS cases
scripts/collect-sar-matrix.sh centos-6.5  # Collect one case again
```

Each run creates `target/sar-matrix/<UTC timestamp>/`, containing each case's `sa`, `sar-A.txt`, provenance, SHA-256 and log, plus the combined `SUMMARY.tsv`. Failed cases are recorded, remaining cases continue, and the overall run exits nonzero. See the [runbook](format/06-live-matrix.md) for prerequisites, collection settings and rootfs/RPM mappings.
