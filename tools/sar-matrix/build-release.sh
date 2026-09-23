#!/bin/sh
set -eu
mkdir -p /opt/sysstat-source
if [ "$SRPM_MEMBER" != - ]; then
    mkdir /tmp/srpm
    cd /tmp/srpm
    rpm2cpio /tmp/source.archive | cpio -idm "$SRPM_MEMBER"
    printf '%s  %s\n' "$MEMBER_SHA256" "$SRPM_MEMBER" | sha256sum -c -
    tar -xf "$SRPM_MEMBER" --strip-components=1 -C /opt/sysstat-source
else
    tar -xf /tmp/source.archive --strip-components=1 -C /opt/sysstat-source
fi
cd /opt/sysstat-source
if [ "$SYSSTAT_VERSION" = 9.0.0 ]; then
    # Backport the two null-after-free assignments from official 9.0.1.
    # CPU and CPU-frequency activities share a bitmap in 9.0.0; otherwise sar
    # aborts on a double free. Record layouts and accounting are unchanged.
    sed -i \
        -e 's/free(act\[i\]->buf\[j\]);/free(act[i]->buf[j]); act[i]->buf[j] = NULL;/' \
        -e 's/free(act\[i\]->bitmap->b_array);/free(act[i]->bitmap->b_array); act[i]->bitmap->b_array = NULL;/' \
        sa_common.c
fi
# Compatibility with contemporary glibc and GCC. These adaptations preserve
# accounting expressions and on-disk structures. Record them in provenance.
cflags='-O2 -fcommon -fgnu89-inline -include sys/sysmacros.h'
case "$SYSSTAT_VERSION" in
    [2-8].*)
        mkdir -p /opt/sysstat-compat/asm
        printf '%s\n' '#include <unistd.h>' \
            '#define PAGE_SIZE (sysconf(_SC_PAGESIZE))' \
            '#define PAGE_SHIFT (__builtin_ctzl((unsigned long)sysconf(_SC_PAGESIZE)))' \
            > /opt/sysstat-compat/asm/page.h
        printf '%s\n' '#include <sys/sysmacros.h>' \
            '#ifndef SA_DIR' '#define SA_DIR "/var/log/sa"' '#endif' \
            '#ifndef SADC_PATH' '#define SADC_PATH "/opt/sysstat-source/sadc"' '#endif' \
            '#ifndef MAX_BLKDEV' '#define MAX_BLKDEV 255' '#endif' \
            > /opt/sysstat-compat/compat.h
        cflags='-O2 -fcommon -fgnu89-inline -I/opt/sysstat-compat -include /opt/sysstat-compat/compat.h'
        if [ -f sa.h ]; then sed -i 's/unsigned int \[\]\[\]/unsigned int [][NR_IRQS]/g' sa.h; fi
        sed -i 's/(char \*) buffer += \([^;]*\);/buffer = (char *) buffer + \1;/g' sar.c
        ;;
esac
printf '%s\n' "$cflags" > resarch-build-cflags.txt
if [ -x ./configure ]; then
    CFLAGS="$cflags" ./configure --disable-nls --prefix=/opt/sysstat
fi
# Old archive recipes update the same .a concurrently; serialize those builds.
case "$SYSSTAT_VERSION" in
    2.2) make -j1 sar "CFLAGS=$cflags" REQUIRE_NLS= DFLAGS= ;;
    [3-8].*) make -j1 sar sadc "CFLAGS=$cflags" REQUIRE_NLS= DFLAGS= ;;
    *) make -j2 sar sadc "CFLAGS=$cflags" REQUIRE_NLS= ;;
esac
# Run directly from the build directory: very old install targets are interactive
# and install unrelated cron jobs. sadf is optional in releases predating it.
if [ -f sadf.c ]; then
    # Some 11.2.x releases use LC_NUMERIC outside the NLS conditional but only
    # include locale.h when NLS is enabled. Supplying its declarations is enough.
    make -j1 sadf "CFLAGS=$cflags -include locale.h" REQUIRE_NLS=
fi
# Keep the compiler and GPL sources in the build stage, not every runtime image.
mkdir -p /opt/sysstat-runtime
cp sar resarch-build-cflags.txt /opt/sysstat-runtime/
for program in sadc sadf; do
    if [ -f "$program" ]; then cp "$program" /opt/sysstat-runtime/; fi
done
