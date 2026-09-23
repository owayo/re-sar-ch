#!/bin/sh

set -eu

export LC_ALL=C
export TZ=UTC

case_id=${CASE_ID:-unknown}
source_image=${SOURCE_IMAGE:-unknown}
interval=${SAR_INTERVAL:-1}
count=${SAR_COUNT:-2}
output_dir=/out

case "$interval" in
    ''|*[!0-9]*|0)
        printf '%s\n' "SAR_INTERVAL must be a positive integer: $interval" >&2
        exit 2
        ;;
esac

case "$count" in
    ''|*[!0-9]*|0|1)
        printf '%s\n' "SAR_COUNT must be an integer greater than one: $count" >&2
        exit 2
        ;;
esac

[ -d "$output_dir" ] || {
    printf '%s\n' "$output_dir is not mounted" >&2
    exit 2
}

# The launcher fixes the container identity to keep the generated nodename
# deterministic.  Retain this as a fallback for other compatible runtimes.
if command -v hostname >/dev/null 2>&1; then
    hostname resarch-fixture 2>/dev/null || :
fi

sadc_path=
if command -v sadc >/dev/null 2>&1; then
    sadc_path=$(command -v sadc)
else
    for candidate in \
        /opt/sysstat/lib/sa/sadc \
        /opt/sysstat/lib64/sa/sadc \
        /usr/lib/sysstat/sadc \
        /usr/libexec/sysstat/sadc \
        /usr/lib/sa/sadc \
        /usr/lib64/sa/sadc
    do
        if [ -x "$candidate" ]; then
            sadc_path=$candidate
            break
        fi
    done
fi

[ -n "$sadc_path" ] || {
    printf '%s\n' 'sadc was not found after installing sysstat' >&2
    exit 1
}

sysstat_version=$(sar -V 2>&1)
sysstat_version=$(printf '%s\n' "$sysstat_version" | sed -n '1p')

printf '%s\n' "collecting $case_id ($sysstat_version): ${count} samples, ${interval}s interval"
"$sadc_path" -S XALL "$interval" "$count" "$output_dir/sa"

sar -A -C -t -f "$output_dir/sa" > "$output_dir/sar-A.txt"

optional_failures=
capture_optional() {
    destination=$1
    shift
    if "$@" > "$output_dir/$destination" 2> "$output_dir/$destination.stderr"; then
        return 0
    else
        status=$?
    fi

    optional_failures="${optional_failures}${destination}=${status}\n"
    return 0
}

capture_optional sadf-H.txt sadf -H "$output_dir/sa"
capture_optional sadf-d.txt sadf -d "$output_dir/sa" -- -A
capture_optional sadf-p.txt sadf -p "$output_dir/sa" -- -A
capture_optional sadf-r.txt sadf -r "$output_dir/sa" -- -A
capture_optional sadf-j.json sadf -j "$output_dir/sa" -- -A
capture_optional sadf-x.xml sadf -x "$output_dir/sa" -- -A

{
    printf 'case_id=%s\n' "$case_id"
    printf 'source_image=%s\n' "$source_image"
    printf 'sysstat=%s\n' "$sysstat_version"
    if [ -n "${SYSSTAT_GIT_COMMIT:-}" ]; then
        printf 'source_commit=%s\n' "$SYSSTAT_GIT_COMMIT"
    fi
    if [ -n "${SYSSTAT_CFLAGS:-}" ]; then
        printf 'source_cflags=%s\n' "$SYSSTAT_CFLAGS"
    fi
    printf 'sadc=%s\n' "$sadc_path"
    printf 'interval_seconds=%s\n' "$interval"
    printf 'sample_count=%s\n' "$count"
    printf 'architecture=%s\n' "$(uname -m)"
    printf 'long_bits=%s\n' "$(getconf LONG_BIT 2>/dev/null || printf unknown)"
    printf 'kernel=%s\n' "$(uname -sr)"
    if command -v sha256sum >/dev/null 2>&1; then
        sa_sha256=$(sha256sum "$output_dir/sa")
        sa_sha256=${sa_sha256%% *}
        printf 'sa_sha256=%s\n' "$sa_sha256"
    fi
    if [ -n "$optional_failures" ]; then
        printf 'optional_failures_begin\n%boptional_failures_end\n' "$optional_failures"
    fi
} > "$output_dir/PROVENANCE.txt"

if [ -r /etc/os-release ]; then
    cp /etc/os-release "$output_dir/os-release.txt"
fi
