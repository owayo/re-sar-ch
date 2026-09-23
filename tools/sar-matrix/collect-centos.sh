#!/bin/sh

set -eu
export LC_ALL=C TZ=UTC

[ "$(uname -n)" = resarch-fixture ]
[ -e /etc/mtab ] || ln -s /proc/mounts /etc/mtab
if [ "$DEP_FILE" = - ]; then
    rpm -Uvh --nodeps "/rpms/$RPM_FILE" > /out/rpm-install.txt 2>&1
else
    rpm -Uvh --nodeps "/rpms/$DEP_FILE" "/rpms/$RPM_FILE" > /out/rpm-install.txt 2>&1
fi

sadc_path=$(rpm -ql sysstat | sed -n '\|/sadc$|p')
[ -x "$sadc_path" ]
cat /etc/redhat-release > /out/rootfs-release.txt

case "$CASE_ID" in
    centos-3.9|centos-4.9|centos-5.11)
        "$sadc_path" "$SAR_INTERVAL" "$SAR_COUNT" /out/sa
        sar -A -t -f /out/sa > /out/sar-A.txt
        sar -A -t -f /out/sa > /out/sar-repeat.txt
        sadc_options=default
        sar_options='-A -t'
        ;;
    *)
        "$sadc_path" -S XALL "$SAR_INTERVAL" "$SAR_COUNT" /out/sa
        sar -A -C -t -f /out/sa > /out/sar-A.txt
        sar -A -C -t -f /out/sa > /out/sar-repeat.txt
        sadc_options='-S XALL'
        sar_options='-A -C -t'
        ;;
esac

{
    printf 'case_id=%s\nsource_image=%s\n' "$CASE_ID" "$SOURCE_IMAGE"
    printf 'sysstat=%s\n' "$(rpm -q --qf '%{VERSION}-%{RELEASE}' sysstat)"
    printf 'rootfs=%s\n' "$(cat /etc/redhat-release)"
    printf 'kernel=%s\narchitecture=%s\n' "$(uname -sr)" "$(uname -m)"
    printf 'long_bits=%s\nhostname=%s\n' "$(getconf LONG_BIT)" "$(uname -n)"
    printf 'locale=C\ntimezone=UTC\n'
    printf 'sadc=%s\nsadc_options=%s\nsar_options=%s\n' "$sadc_path" "$sadc_options" "$sar_options"
    printf 'interval_seconds=%s\nsample_count=%s\n' "$SAR_INTERVAL" "$SAR_COUNT"
    printf 'collected_at_utc=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
} > /out/PROVENANCE.txt
