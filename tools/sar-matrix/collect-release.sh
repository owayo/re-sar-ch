#!/bin/sh
set -eu
export LC_ALL=C TZ=UTC
cd /opt/sysstat-source
export PATH="/opt/sysstat-source:$PATH"
# Discover supported collection categories from this release's own usage text.
# Do not retry a failed collection with fewer activities: that would hide errors.
help=$(./sadc -? 2>&1 || :)
case "$help" in
    *-S*)
        case "$help" in
            *XALL*) set -- -S XALL ;;
            *ALL*) set -- -S ALL ;;
            *) printf '%s\n' 'Cannot determine sadc -S collection categories' >&2; exit 1 ;;
        esac
        ;;
    *)
        case "$SYSSTAT_VERSION" in
            [3-5].*|6.0.*) set -- -I ;;
            [6-8].*) set -- -I -d ;;
            *) set -- ;;
        esac
        ;;
esac
sadc_options="$*"
rm -f /out/sa
if [ "$SYSSTAT_VERSION" = 2.2 ]; then
    ./sar -A -I -2 -U -1 -o /out/sa "$SAR_INTERVAL" "$SAR_COUNT" > /out/collection-sar.txt
else
    ./sadc "$@" "$SAR_INTERVAL" "$SAR_COUNT" /out/sa
fi
case "$SYSSTAT_VERSION" in
    3.2.4) set -- -A ;;
    [2-8].*) set -- -A -t ;;
    *) set -- -A -C -t ;;
esac
if [ "$SYSSTAT_VERSION" = 2.2 ]; then
    ./sar -A -I -2 -U -1 -f /out/sa 1 > /out/sar-A.txt
    ./sar -A -I -2 -U -1 -f /out/sa 1 > /out/sar-repeat.txt
else
    ./sar "$@" -f /out/sa > /out/sar-A.txt
    ./sar "$@" -f /out/sa > /out/sar-repeat.txt
fi
cmp /out/sar-A.txt /out/sar-repeat.txt
optional_failures=
for flag in H d p r j x; do
    case "$flag" in
        j) extension=json ;;
        x) extension=xml ;;
        *) extension=txt ;;
    esac
    destination="sadf-$flag.$extension"
    if [ "$flag" = H ]; then set --; else set -- -- -A; fi
    if ./sadf "-$flag" /out/sa "$@" > "/out/$destination" 2> "/out/$destination.stderr"; then
        :
    else
        optional_failures="${optional_failures}${destination}=$?\n"
    fi
done
{
    printf 'case_id=%s\nsource_image=%s\n' "$CASE_ID" "$SOURCE_IMAGE"
    printf 'sysstat=sysstat version %s\nsource_version=%s\n' "$SYSSTAT_VERSION" "$SYSSTAT_VERSION"
    printf 'native_version_output=%s\n' "$(./sar -V 2>&1 | sed -n '1p')"
    printf 'source_commit=%s\nsource_url=%s\nsource_sha256=%s\n' "$SYSSTAT_GIT_COMMIT" "$SYSSTAT_SOURCE_URL" "$SYSSTAT_SOURCE_SHA256"
    printf 'source_cflags=%s\n' "$(cat /opt/sysstat-source/resarch-build-cflags.txt)"
    if [ "$SYSSTAT_VERSION" = 9.0.0 ]; then
        printf 'native_backport=9.0.1 null-after-free fixes in free_structures/free_bitmaps\n'
    fi
    case "$SYSSTAT_VERSION" in
        [2-8].*) printf 'build_compat=page-size shim, default paths, complete IRQ prototype, pointer lvalue syntax; see build-release.sh\n' ;;
    esac
    printf 'interval_seconds=%s\nsample_count=%s\n' "$SAR_INTERVAL" "$SAR_COUNT"
    if [ "$SYSSTAT_VERSION" = 2.2 ]; then
        printf 'collector=sar -A -I -2 -U -1 -o\n'
    else
        printf 'collector=sadc\nsadc_options=%s\n' "$sadc_options"
    fi
    printf 'architecture=%s\nlong_bits=%s\nkernel=%s\n' "$(uname -m)" "$(getconf LONG_BIT)" "$(uname -sr)"
    printf 'sar_repeat_matches=true\n'
    if [ -n "$optional_failures" ]; then
        printf 'optional_failures_begin\n%boptional_failures_end\n' "$optional_failures"
    fi
} > /out/PROVENANCE.txt
cp /etc/os-release /out/os-release.txt
