#!/usr/bin/env python3
"""Exercise launcher failure/recovery behavior without containers or network."""

import csv
import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parents[2]


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sar matrix test ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repository"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copyfile(
            REPOSITORY / "scripts/collect-sar-matrix.sh",
            self.repo / "scripts/collect-sar-matrix.sh",
        )
        matrix = self.repo / "tools/sar-matrix"
        matrix.mkdir(parents=True)
        for name in (
            "Containerfile.compiler",
            "Containerfile.release",
            "build-release.sh",
        ):
            (matrix / name).write_text("# test fixture\n")
        (matrix / "centos.tsv").write_text("# no RPM cases in this isolated test\n")
        source = b"independently generated source-cache fixture\n"
        self.cache = self.repo / "target/sar-matrix/sources/upstream-6.1.1.archive"
        self.cache.parent.mkdir(parents=True)
        self.cache.write_bytes(source)
        row = [
            "upstream-6.1.1",
            "6.1.1",
            "https://invalid.example/source",
            hashlib.sha256(source).hexdigest(),
            "-",
            "-",
            "-",
        ]
        (matrix / "upstream.tsv").write_text("\t".join(row) + "\n")
        tools = self.root / "bin"
        tools.mkdir()
        target = self.root / "target"
        (target / "debug/examples").mkdir(parents=True)
        for name in ("resarch", "examples/scan", "examples/verify_snapshot"):
            shutil.copyfile(shutil.which("true"), target / "debug" / name)
            (target / "debug" / name).chmod(0o755)
        self.marker = self.root / "image-created"
        self.log = self.root / "runtime.log"
        self.env = dict(
            os.environ,
            PATH=str(tools) + ":" + os.environ["PATH"],
            CONTAINER_RUNTIME="container",
            SAR_MATRIX_KEEP_IMAGES="0",
            SAR_MATRIX_RESUME="0",
            SAR_INTERVAL="1",
            SAR_COUNT="2",
            FAKE_TARGET=str(target),
            FAKE_MARKER=str(self.marker),
            FAKE_LOG=str(self.log),
        )
        self.env.pop("SAR_COMPILER_IMAGE", None)
        (tools / "cargo").write_text("""#!/bin/sh
if [ "$1" = metadata ]; then printf '{"target_directory":"%s"}\\n' "$FAKE_TARGET"; fi
""")
        (tools / "container").write_text("""#!/bin/bash
set -eu
printf '%s\\n' "$*" >> "$FAKE_LOG"
case "$1" in
    system) printf '%s\\n' 'status              running' ;;
    --version) printf '%s\\n' mock-runtime ;;
    image)
        case "$2" in
            inspect)
                case "$3" in *compiler*) ;; *) [ -f "$FAKE_MARKER" ] || exit 1 ;; esac
                printf '%s\\n' '[{"Id":"sha256:test-compiler"}]'
                ;;
            rm)
                if [ "${FAKE_REMOVE_FAILURE:-0}" = 1 ]; then
                    printf 'simulated cleanup failure\\n' >&2; exit 44
                fi
                rm "$FAKE_MARKER"
                ;;
        esac ;;
    build) touch "$FAKE_MARKER" ;;
    run) exit 23 ;;
    *) exit 24 ;;
esac
""")
        for path in tools.iterdir():
            path.chmod(0o755)

    def run_matrix(self, mode="upstream-6.1.1", **settings):
        output = self.root / "output"
        env = dict(self.env, SAR_MATRIX_OUTPUT=str(output), **settings)
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/collect-sar-matrix.sh"), mode],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            universal_newlines=True,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        with (output / "SUMMARY.tsv").open() as stream:
            rows = list(csv.DictReader(stream, delimiter="\t"))
        return rows, output

    def test_continue_after_every_failed_case(self):
        rows, _ = self.run_matrix("distro")
        self.assertEqual(len(rows), 5)
        self.assertTrue(all(row["status"] == "failed:23" for row in rows))

    def test_corrupt_cache_is_rejected_before_execution(self):
        self.cache.write_bytes(b"corrupt cache")
        _, output = self.run_matrix()
        log = (output / "upstream-6.1.1/collection.log").read_text()
        self.assertIn("SHA-256 mismatch", log)
        self.assertNotIn("run ", self.log.read_text())

    def test_new_image_is_removed_on_collection_failure(self):
        rows, _ = self.run_matrix()
        self.assertEqual(rows[0]["status"], "failed:23")
        self.assertFalse(self.marker.exists())

    def test_opt_in_keeps_new_image(self):
        self.run_matrix(SAR_MATRIX_KEEP_IMAGES="1")
        self.assertTrue(self.marker.exists())

    def test_preexisting_image_is_not_removed(self):
        self.marker.touch()
        self.run_matrix()
        self.assertTrue(self.marker.exists())

    def test_cleanup_failure_is_reported_without_hiding_original_failure(self):
        rows, output = self.run_matrix(FAKE_REMOVE_FAILURE="1")
        self.assertEqual(rows[0]["status"], "failed:23")
        log = (output / "upstream-6.1.1/collection.log").read_text()
        self.assertIn("simulated cleanup failure", log)
        self.assertIn(
            "WARNING: release image cleanup failed: resarch-sysstat-6.1.1:", log
        )

    def test_incomplete_decode_fails_even_when_eof_scan_succeeds(self):
        case = self.root / "output/upstream-6.1.1"
        case.mkdir(parents=True)
        for name in ("sa", "sar-A.txt"):
            (case / name).write_bytes(b"independent launcher fixture\n")
        (case / "PROVENANCE.txt").write_text("sysstat=sysstat version 6.1.1\n")
        (case / "SHA256SUMS").write_text(
            "".join(
                hashlib.sha256((case / name).read_bytes()).hexdigest()
                + "  "
                + name
                + "\n"
                for name in ("sa", "sar-A.txt")
            )
        )
        target = self.root / "target/debug"
        (target / "resarch").write_text("""#!/bin/sh
if [ "$1" = identify ]; then
    printf '%s\\n' '[{"format_magic":"0x2168","readable":true}]'
else
    printf 'unexpected render\\n' >> "$FAKE_LOG"
    exit 99
fi
""")
        (target / "examples/scan").write_text("""#!/bin/sh
printf 'scan: end_offset=1 file_size=1 trailing=0 exact=true\\n'
""")
        (target / "examples/verify_snapshot").write_text("""#!/bin/sh
printf 'activities: declared=2 planned=1 skipped=1\\n'
exit 42
""")
        rows, output = self.run_matrix(SAR_MATRIX_RESUME="1")
        self.assertEqual(rows[0]["status"], "failed:42")
        self.assertIn(
            "skipped=1", (output / "upstream-6.1.1/resarch-decode.txt").read_text()
        )
        self.assertNotIn("unexpected render", self.log.read_text())


if __name__ == "__main__":
    unittest.main()
