#!/usr/bin/env python3
"""Refresh official Git release pins; preserve archived pre-Git source records.

Run from any directory. Downloaded source archives stay under target/ (not Git).
A moved existing release tag is rejected rather than silently changing its pin.
"""
import concurrent.futures
import hashlib
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / 'tools/sar-matrix/upstream.tsv'
CACHE = ROOT / 'target/sar-matrix/sources'


def main():
    CACHE.mkdir(parents=True, exist_ok=True)
    old = [line.split('\t') for line in MANIFEST.read_text().splitlines()
           if line and not line.startswith('#')]
    known = {row[1]: row for row in old}
    output = subprocess.check_output(
        ['git', 'ls-remote', '--tags', 'https://github.com/sysstat/sysstat.git'],
        universal_newlines=True)
    tags = {}
    for line in output.splitlines():
        commit, ref = line.split()
        tag = ref.rsplit('/', 1)[-1]
        name = tag.replace('^{}', '')
        if not re.fullmatch(r'v?\d+(?:\.\d+){2,3}', name):
            raise SystemExit('Unrecognized official tag: ' + name)
        if name not in tags or tag.endswith('^{}'):
            tags[name] = commit
    releases = {}
    for tag, commit in tags.items():
        version = tag.lstrip('v')
        if version in releases and releases[version] != commit:
            raise SystemExit('Different commits for version aliases: ' + version)
        if version in known and known[version][4] != commit:
            raise SystemExit('Existing release tag moved: ' + tag)
        releases[version] = commit

    def fetch(item):
        version, commit = item
        case = 'upstream-' + version
        url = 'https://codeload.github.com/sysstat/sysstat/tar.gz/' + commit
        destination = CACHE / (case + '.archive')
        if not destination.exists():
            part = destination.with_suffix('.part')
            subprocess.check_call(['curl', '-fL', '--retry', '3', '--connect-timeout',
                                   '20', '--max-time', '180', url, '-o', str(part)])
            part.replace(destination)
        digest = hashlib.sha256(destination.read_bytes()).hexdigest()
        if version in known and known[version][3] != digest:
            raise SystemExit('Cached source SHA-256 mismatch: ' + case)
        return [case, version, url, digest, commit, '-', '-']

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(fetch, releases.items()))
    rows.extend(row for row in old if row[4] == '-')
    rows.sort(key=lambda row: tuple(map(int, row[1].split('.'))))
    text = '# case\tversion\turl\tsha256\tgit_commit\tsrpm_member\tmember_sha256\n'
    text += ''.join('\t'.join(row) + '\n' for row in rows)
    MANIFEST.write_text(text)
    print('{} Git tags, {} Git releases, {} total cases'.format(
        len(tags), len(releases), len(rows)))


if __name__ == '__main__':
    main()
