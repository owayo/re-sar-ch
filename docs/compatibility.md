# What "sar compatible" means here

The phrase covers three separate things, and reSARch states each one explicitly rather than implying all of them:

| Axis | Scope |
|---|---|
| **Input compatibility** | The 28 [supported formats](formats.md), with legacy field and ABI limitations |
| **Computation and output** | Which `sar` / `sadf` version's rendering is reproduced |
| **CLI compatibility** | Which options and calling conventions are accepted |

The `resarch` binary does not perform live sampling (`sadc`). The bundled collection script runs native `sadc` inside each image. Binary output from `resarch` is limited to re-encoding a file it just read (`sadf -c`), leaving every value alone. Text and charts can be saved with `sa2sar`, `detect --svg-dir`, and the other output commands. The generation of the file being read and the output format being reproduced are separate settings. By default every supported format uses the `sysstat` 12.8.0 rendering profile; `--sar-profile` selects another `sar` to reproduce instead (see below).

Options that are parsed but not yet acted upon are rejected at run time with a reason:

| Option | Status |
|---|---|
| `sar -o` / `--sadc` | Collection is out of scope — rejected explicitly |
| `sadf -l` | PCP output is not implemented |
| `sadf -g -O autoscale,packed,customcol` | These SVG options are explicitly rejected; skipempty/showidle/showinfo/showtoc/height/bwcol/debug/oneday are supported |
| `sadf -H` combined with another format | Not implemented — use `resarch sadf -H <file>` |

## Reproducing RHEL / CentOS 7's `sar`

```bash
resarch sa2sar sa13 --sar-profile sysstat-10.1.5-el7 -o sar13
resarch --sar-profile sysstat-10.1.5-el7 -A -f sa13        # the sar entry point, el7 grammar
resarch --sar-profile sysstat-10.1.5-el7 -R -f sa13 --sar-page-size 65536   # a ppc64le host
```

| `--sar-profile` | Reproduces | Reads |
|---|---|---|
| `current` (= `sysstat-12.8.0`, default) | upstream `sysstat` 12.8.0 | every supported input format |
| `sysstat-10.1.5-el7` | RHEL / CentOS 7's `sysstat-10.1.5-17.el7` … `-20.el7_9` | `format_magic` 0x2171 only, as 10.1.5 itself |

A report written by a RHEL 7 host differs from the default rendering in more than column headers, so the profile reproduces the whole of that `sar`:

- **Columns**: `-B` ends with `%vmeff`, `-d` shows `rd_sec/s … svctm`, `-A` includes `-R` (`frmpg/s bufpg/s campg/s`), `-r` has ten columns, `-n DEV` has no `%ifutil`
- **Arithmetic**: the CPU `all` row divides the file's aggregate slot by the record header's uptime instead of re-summing the CPUs; offline CPUs print `0.00` and carry their last values forward; some averages divide integers before converting to floating point (a `kbswpcad` average of 15.5 prints as `15`, not `16`)
- **Layout**: `LINUX RESTART` lines carry no CPU count, `COM` lines keep their text as is, and the header repeats every 11 samples under `-A` because Red Hat raised `NR_CPUS` to 8192
- **Grammar** (on the `sar` entry point): `-h` is help, `-R` exists, `-I` takes `XALL` and interrupt numbers; options that 10.1.5 does not have are usage errors

The profile is never picked automatically. A file's header records the `sadc` that wrote it, not the `sar` that `sa2` ran or the patches it was built with, and switching the output on a guess would change reports behind your back. Two inputs cannot come from the file at all: `-R` converts kilobytes to pages with the page size of the host running `sar` (`--sar-page-size`, default 4096 — the x86_64 value), and `-p` / `-j` look up device names on that host, which reSARch does not do for a log collected elsewhere (`dev<major>-<minor>` stays). `sadf` has no profile and refuses the option.

This was checked against a RHEL 7 host's own reports — 29 days of `sa` binaries and the `sarDD` its `sa2` wrote match byte for byte — and against el7's `sar` built from the CentOS source RPM, on real files and on synthetic ones covering every activity and the corner cases (offline CPUs, counter wrap, interface and disk re-registration, restarts, comments). The rules and the verification procedure are in [`docs/format/05-sysstat-10.1.5-el7.md`](format/05-sysstat-10.1.5-el7.md).

## Environment variables `sar` reads

Upstream `sar` takes part of its formatting from the environment, so reSARch reads the same variables for its compatible output (`sar`, `sa2sar`, `show --format sar`).

| Variable | Accepted value | Effect |
|---|---|---|
| `S_TIME_FORMAT` | exactly `ISO` | Banner date becomes `YYYY-MM-DD` instead of `MM/DD/YY` |
| `S_REPEAT_HEADER` | all digits, `> 0` | Reprints column headers every N lines — **only when stdout is not a terminal** |

Matching is as strict as upstream's: `S_TIME_FORMAT=iso` does nothing, and a `S_REPEAT_HEADER` with a sign, spaces, or non-digits is ignored rather than rejected.

When stdout *is* a terminal, the header interval comes from the window height (`rows - 2`), and `S_REPEAT_HEADER` is not consulted — the same `else if` that upstream has. With neither source available the interval is 86400 lines.

That interval is not as large as it looks. Upstream counts a sample as `count_bits(cpu_bitmap)` lines for the activities that use the CPU bitmap, and `-A` and `-P ALL` fill the whole bitmap, so each sample counts as 8200 lines regardless of how many CPUs the host has. `sar -A` therefore reprints its CPU header every 11 samples even in a pipe. reSARch reproduces this.

These variables do not affect `sadf`: upstream never sets `S_F_PREFD_TIME_OUTPUT` there, so its dates stay `%Y-%m-%d` and its timestamps stay `%H:%M:%S` regardless. Under `--sar-profile sysstat-10.1.5-el7`, `S_REPEAT_HEADER` is not read either — that variable arrived after 10.1.5 — while `S_TIME_FORMAT` works as described.
