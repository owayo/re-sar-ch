#!/bin/bash

set -euo pipefail

usage() {
    printf '%s\n' 'Usage: scripts/collect-sar-matrix.sh MODE'
    printf '%s\n' ''
    printf '%s\n' 'Modes:'
    printf '%s\n' '  latest       Build and collect upstream sysstat 12.8.0'
    printf '%s\n' '  distro       Collect packages from five Linux distributions'
    printf '%s\n' '  generations  Collect upstream 10.2.1, 11.6.6, 12.0.6, 12.8.0'
    printf '%s\n' '  centos       Collect eleven CentOS Vault RPM releases (3.9 to 8.5)'
    printf '%s\n' '  all          Run all twenty recorded distro/upstream cases'
    printf '%s\n' '  upstream-all Build and verify every pinned official source release'
    printf '%s\n' '  CASE         Run one case printed by --list'
    printf '%s\n' '  --list       Print available cases without running containers'
    printf '%s\n' ''
    printf '%s\n' 'Environment:'
    printf '%s\n' '  CONTAINER_RUNTIME=container|docker  Override automatic detection'
    printf '%s\n' '  SAR_INTERVAL=1                     Seconds between samples'
    printf '%s\n' '  SAR_COUNT=2                        Number of samples (at least 2)'
    printf '%s\n' '  SAR_MATRIX_OUTPUT=path             Output root (must not exist)'
    printf '%s\n' '  SAR_MATRIX_RESUME=1                Reuse and reverify previously collected cases'
    printf '%s\n' '  SAR_COMPILER_IMAGE=image           Use an already provisioned compiler image'
    printf '%s\n' '  SAR_MATRIX_KEEP_IMAGES=1           Retain new release images (default: remove after each case)'
}

list_cases() {
    printf '%s\n' \
        alpine-3.23 \
        debian-13 \
        ubuntu-24.04 \
        fedora-44 \
        rockylinux-9 \
        upstream-10.2.1 \
        upstream-11.6.6 \
        upstream-12.0.6 \
        upstream-12.8.0
    awk -F '\t' '!/^#/ {print $1}' "$matrix_dir/centos.tsv"
    awk -F '\t' '!/^#/ && $1 != "upstream-10.2.1" && $1 != "upstream-11.6.6" && $1 != "upstream-12.0.6" && $1 != "upstream-12.8.0" {print $1}' "$matrix_dir/upstream.tsv"
}

die() {
    printf '%s\n' "$1" >&2
    exit "${2:-1}"
}

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
matrix_dir="$repo_dir/tools/sar-matrix"
mode=${1:-latest}
interval=${SAR_INTERVAL:-1}
count=${SAR_COUNT:-2}
keep_images=${SAR_MATRIX_KEEP_IMAGES:-0}

if [ "$mode" = --help ] || [ "$mode" = -h ]; then
    usage
    exit 0
fi

if [ "$mode" = --list ]; then
    list_cases
    exit 0
fi

case "$interval" in
    ''|*[!0-9]*|0) die "SAR_INTERVAL must be a positive integer: $interval" 2 ;;
esac
case "$count" in
    ''|*[!0-9]*|0|1) die "SAR_COUNT must be an integer greater than one: $count" 2 ;;
esac
case "$keep_images" in
    0|1) ;;
    *) die "SAR_MATRIX_KEEP_IMAGES must be 0 or 1: $keep_images" 2 ;;
esac

# Resolve the selection before creating output or starting containers.
case "$mode" in
    latest) selected_cases=upstream-12.8.0 ;;
    distro) selected_cases=$(list_cases | sed -n '1,5p') ;;
    generations) selected_cases=$(list_cases | sed -n '6,9p') ;;
    centos) selected_cases=$(awk -F '\t' '!/^#/ {print $1}' "$matrix_dir/centos.tsv") ;;
    all) selected_cases=$(list_cases | sed -n '1,20p') ;;
    upstream-all) selected_cases=$(awk -F '\t' '!/^#/ {print $1}' "$matrix_dir/upstream.tsv") ;;
    *)
        list_cases | awk -v wanted="$mode" '$0 == wanted {found=1} END {exit !found}' \
            || die "Unknown case: $mode (run with --list)" 2
        selected_cases=$mode
        ;;
esac
for required in cargo jq curl shasum; do
    command -v "$required" >/dev/null 2>&1 || die "Required command not found: $required"
done

runtime=${CONTAINER_RUNTIME:-}
if [ -z "$runtime" ]; then
    if command -v container >/dev/null 2>&1; then
        runtime=container
    elif command -v docker >/dev/null 2>&1; then
        runtime=docker
    else
        die 'Neither Apple container nor Docker was found on PATH.'
    fi
fi

case "$runtime" in
    container|docker) ;;
    *) die "Unsupported CONTAINER_RUNTIME: $runtime" 2 ;;
esac

identity_args=(--name resarch-fixture)
if [ "$runtime" = docker ]; then
    identity_args+=(--hostname resarch-fixture)
fi

if [ "$runtime" = container ]; then
    container_status=$(container system status 2>&1 || true)
    case "$container_status" in
        *'status              running'*) ;;
        *) die "Apple Container is not running. Run: container system start\n$container_status" ;;
    esac
fi

timestamp=$(date -u '+%Y%m%dT%H%M%SZ')
run_root=${SAR_MATRIX_OUTPUT:-"$repo_dir/target/sar-matrix/$timestamp"}
if [ -e "$run_root" ] && [ "${SAR_MATRIX_RESUME:-0}" != 1 ]; then
    die "Output already exists; choose a new SAR_MATRIX_OUTPUT or SAR_MATRIX_RESUME=1: $run_root"
fi
mkdir -p "$run_root"
run_root=$(CDPATH='' cd -- "$run_root" && pwd)
rpm_cache="$repo_dir/target/sar-matrix/rpms"
source_cache="$repo_dir/target/sar-matrix/sources"
mkdir -p "$rpm_cache" "$source_cache"
compiler_hash=$(shasum -a 256 "$matrix_dir/Containerfile.compiler")
compiler_image=${SAR_COMPILER_IMAGE:-"resarch-sysstat-compiler:${compiler_hash:0:16}"}

printf 'runtime=%s\nmode=%s\ninterval_seconds=%s\nsample_count=%s\n' \
    "$runtime" "$mode" "$interval" "$count" > "$run_root/RUN.txt"
printf 'case\tsysstat\tformat_magic\tscan_exact\tactivity_decode\tsar_text\tstatus\n' \
    > "$run_root/SUMMARY.tsv"
"$runtime" --version >> "$run_root/RUN.txt"
cargo build --quiet --manifest-path "$repo_dir/Cargo.toml" --bin resarch --example scan --example verify_snapshot
target_dir=$(cargo metadata --no-deps --format-version 1 --manifest-path "$repo_dir/Cargo.toml" | jq -er '.target_directory')
# Use one reader build throughout the matrix, even if another terminal builds
# the repository while collection is running. Avoid per-case Cargo lock waits.
mkdir -p "$run_root/.tools"
cp "$target_dir/debug/resarch" "$run_root/.tools/resarch"
cp "$target_dir/debug/examples/scan" "$run_root/.tools/scan"
cp "$target_dir/debug/examples/verify_snapshot" "$run_root/.tools/verify_snapshot"
resarch_bin="$run_root/.tools/resarch"
scan_bin="$run_root/.tools/scan"
verify_bin="$run_root/.tools/verify_snapshot"
(cd "$run_root/.tools" && shasum -a 256 resarch scan verify_snapshot) > "$run_root/READER-SHA256SUMS"

run_container() {
    case_id=$1
    image=$2
    setup=$3
    case_output="$run_root/$case_id"
    mkdir -p "$case_output"

    printf '%s\n' "==> $case_id ($image)"
    "$runtime" run --rm "${identity_args[@]}" \
        -e "CASE_ID=$case_id" \
        -e "SOURCE_IMAGE=$image" \
        -e "SAR_INTERVAL=$interval" \
        -e "SAR_COUNT=$count" \
        -v "$matrix_dir:/matrix:ro" \
        -v "$case_output:/out" \
        "$image" \
        /bin/sh -eu -c "$setup; exec /bin/sh /matrix/collect.sh"

    verify_case "$case_output"
}

run_upstream() {
    case_id=$1
    local row version source_url source_sha git_commit member member_sha archive build_context build_hash compiler_identity
    row=$(awk -F '\t' -v wanted="$case_id" '$1 == wanted {print}' "$matrix_dir/upstream.tsv")
    [ -n "$row" ] || die "No pinned source for $case_id"
    IFS=$'\t' read -r case_id version source_url source_sha git_commit member member_sha <<< "$row"
    case_output="$run_root/$case_id"
    mkdir -p "$case_output"
    archive="$source_cache/$case_id.archive"
    fetch_rpm "$source_url" "$source_sha" "$archive"
    if ! "$runtime" image inspect "$compiler_image" >/dev/null 2>&1; then
        [ -z "${SAR_COMPILER_IMAGE:-}" ] || die "Custom compiler image is not available locally: $compiler_image"
        "$runtime" build -t "$compiler_image" -f "$matrix_dir/Containerfile.compiler" "$matrix_dir"
    fi
    build_context="$repo_dir/target/sar-matrix/build/$case_id"
    mkdir -p "$build_context"
    cp "$archive" "$build_context/source.archive"
    cp "$matrix_dir/build-release.sh" "$matrix_dir/Containerfile.release" "$build_context/"
    compiler_identity=$("$runtime" image inspect "$compiler_image" | jq -er '.[0].Id // .[0].configuration.descriptor.digest')
    build_hash=$({ printf '%s\n' "$row" "$compiler_image" "$compiler_identity"; cat "$matrix_dir/build-release.sh" "$matrix_dir/Containerfile.release"; } | shasum -a 256)
    image="resarch-sysstat-$version:${build_hash:0:16}"
    printf '%s\n' "==> building and collecting $case_id in $image"
    if ! "$runtime" image inspect "$image" >/dev/null 2>&1; then
        if [ "$keep_images" = 0 ]; then
            # Each case runs in its own subshell. Clean up only the release image
            # created by this case, including collection/verification failures.
            trap 'if ! "$runtime" image rm "$image"; then printf "WARNING: release image cleanup failed: %s\n" "$image" >&2; fi' EXIT
        fi
        "$runtime" build -t "$image" -f "$build_context/Containerfile.release" \
            --build-arg "COMPILER_IMAGE=$compiler_image" --build-arg "SYSSTAT_VERSION=$version" \
            --build-arg "SRPM_MEMBER=$member" --build-arg "MEMBER_SHA256=$member_sha" "$build_context"
    fi
    "$runtime" run --rm "${identity_args[@]}" \
        -e "CASE_ID=$case_id" \
        -e "SOURCE_IMAGE=$image" \
        -e "SAR_INTERVAL=$interval" \
        -e "SAR_COUNT=$count" \
        -e "SYSSTAT_VERSION=$version" \
        -e "SYSSTAT_GIT_COMMIT=$git_commit" \
        -e "SYSSTAT_SOURCE_URL=$source_url" -e "SYSSTAT_SOURCE_SHA256=$source_sha" \
        -v "$matrix_dir:/matrix:ro" \
        -v "$case_output:/out" \
        "$image" \
        /bin/sh /matrix/collect-release.sh

    verify_case "$case_output"
}

verify_case() {
    case_output=$1
    printf '%s\n' "    verifying $(basename -- "$case_output") with reSARch"

    [ -s "$case_output/sa" ] && [ -s "$case_output/sar-A.txt" ]
    case_name=$(basename -- "$case_output")
    sysstat=$(sed -n 's/^sysstat=//p' "$case_output/PROVENANCE.txt")
    (cd -- "$case_output" && shasum -a 256 sa sar-A.txt > SHA256SUMS)
    "$resarch_bin" identify "$case_output/sa" --format json > "$case_output/identify.json"
    format_magic=$(jq -r '.[0].format_magic' "$case_output/identify.json")
    readable=$(jq -r '.[0].readable' "$case_output/identify.json")
    [ "$readable" = true ] || die "Unexpected unreadable file: $case_output/sa"
    "$scan_bin" "$case_output/sa" > "$case_output/resarch-scan.txt"
    scan_exact=$(sed -n 's/^scan: .* exact=\([^ ]*\)$/\1/p' "$case_output/resarch-scan.txt")
    [ "$scan_exact" = true ] || die "Scan did not reach exact EOF: $case_output/sa"
    "$verify_bin" "$case_output/sa" > "$case_output/resarch-decode.txt"

    LC_ALL=C TZ=UTC "$resarch_bin" sar -A -C -t -f "$case_output/sa" \
        > "$case_output/resarch-sar-A.txt"

    if cmp -s "$case_output/sar-A.txt" "$case_output/resarch-sar-A.txt"; then
        comparison=exact
        rm -f "$case_output/sar-A.diff"
        printf '%s\n' 'sar_text_comparison=exact' >> "$case_output/PROVENANCE.txt"
    else
        comparison=different
        diff -u "$case_output/sar-A.txt" "$case_output/resarch-sar-A.txt" \
            > "$case_output/sar-A.diff" || true
        printf '%s\n' 'sar_text_comparison=different (see sar-A.diff)' \
            >> "$case_output/PROVENANCE.txt"
    fi

    printf '%s\t%s\t%s\t%s\tcomplete\t%s\tcollected\n' \
        "$case_name" "$sysstat" "$format_magic" "$scan_exact" "$comparison" \
        >> "$run_root/SUMMARY.tsv"
}

fetch_rpm() {
    local url=$1 expected=$2 destination=$3 actual
    if [ ! -f "$destination" ]; then
        curl -fL --retry 3 --connect-timeout 20 --max-time 180 "$url" -o "$destination.part"
        mv -- "$destination.part" "$destination"
    fi
    actual=$(shasum -a 256 "$destination")
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || die "Source SHA-256 mismatch: $destination"
}

run_centos() {
    local row image_tag image_digest rpm_url rpm_sha dep_url dep_sha rpm_file dep_file
    local arch_args
    row=$(awk -F '\t' -v wanted="$1" '$1 == wanted {print}' "$matrix_dir/centos.tsv")
    [ -n "$row" ] || die "Unknown CentOS case: $1"
    IFS=$'\t' read -r case_id image_tag image_digest rpm_url rpm_sha dep_url dep_sha <<< "$row"
    # Pin the registry manifest/index and Vault RPM; the tag is provenance only.
    image="${image_tag%:*}@$image_digest"
    case_output="$run_root/$case_id"
    mkdir -p "$case_output"
    rpm_file=${rpm_url##*/}
    dep_file=-
    fetch_rpm "$rpm_url" "$rpm_sha" "$rpm_cache/$rpm_file"
    if [ "$dep_url" != - ]; then
        dep_file=${dep_url##*/}
        fetch_rpm "$dep_url" "$dep_sha" "$rpm_cache/$dep_file"
    fi
    if [ "$runtime" = container ]; then
        arch_args=(--arch amd64)
        if [ "$(uname -m)" = arm64 ]; then arch_args+=(--rosetta); fi
    else
        arch_args=(--platform linux/amd64)
    fi
    "$runtime" run --rm "${identity_args[@]}" "${arch_args[@]}" \
        -e "CASE_ID=$case_id" -e "SOURCE_IMAGE=$image_tag@$image_digest" \
        -e "RPM_FILE=$rpm_file" -e "DEP_FILE=$dep_file" \
        -e "SAR_INTERVAL=$interval" -e "SAR_COUNT=$count" \
        -v "$matrix_dir:/matrix:ro" -v "$rpm_cache:/rpms:ro" -v "$case_output:/out" \
        "$image" /bin/sh /matrix/collect-centos.sh
    cmp "$case_output/sar-A.txt" "$case_output/sar-repeat.txt"
    {
        printf 'sysstat_url=%s\nsysstat_sha256=%s\n' "$rpm_url" "$rpm_sha"
        printf 'dependency_url=%s\ndependency_sha256=%s\n' "$dep_url" "$dep_sha"
        printf 'runtime=%s\nplatform=linux/amd64\nsar_repeat_matches=true\n' "$runtime"
        printf 'rpm_install_options=-Uvh --nodeps\n'
    } >> "$case_output/PROVENANCE.txt"
    verify_case "$case_output"
}

run_case() {
    case "$1" in
        centos-*) run_centos "$1" ;;
        alpine-3.23)
            run_container "$1" alpine:3.23 'apk add --no-cache sysstat'
            ;;
        debian-13)
            run_container "$1" debian:13-slim \
                'apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends sysstat'
            ;;
        ubuntu-24.04)
            run_container "$1" ubuntu:24.04 \
                'apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends sysstat'
            ;;
        fedora-44)
            run_container "$1" fedora:44 'dnf -y install sysstat'
            ;;
        rockylinux-9)
            run_container "$1" rockylinux:9 'dnf -y install sysstat'
            ;;
        upstream-*) run_upstream "$1" ;;
        *) die "Unknown case: $1 (run with --list)" 2 ;;
    esac
}

failures=0
while IFS= read -r selected_case; do
    mkdir -p "$run_root/$selected_case"
    printf '==> %s (log: %s/%s/collection.log)\n' "$selected_case" "$run_root" "$selected_case"
    # Keep errexit active inside the case. An `if run_case` would disable it in
    # every nested function and could turn a failed collector into apparent success.
    set +e
    (
        set -e
        if [ "${SAR_MATRIX_RESUME:-0}" = 1 ] && [ -s "$run_root/$selected_case/SHA256SUMS" ]; then
            cd "$run_root/$selected_case"
            shasum -a 256 -c SHA256SUMS
            verify_case "$run_root/$selected_case"
        else
            run_case "$selected_case"
        fi
    ) >> "$run_root/$selected_case/collection.log" 2>&1
    case_exit=$?
    set -e
    if [ "$case_exit" -ne 0 ]; then
        failures=$((failures + 1))
        printf '%s\t-\t-\t-\t-\t-\tfailed:%s\n' "$selected_case" "$case_exit" >> "$run_root/SUMMARY.tsv"
        printf '    FAILED (%s); continuing with the remaining cases\n' "$case_exit"
    else
        printf '    collected\n'
    fi
done <<< "$selected_cases"

printf 'completed: %s (failed cases: %s)\n' "$run_root" "$failures"
[ "$failures" -eq 0 ]
