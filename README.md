# kernelctl

[![CI](https://github.com/MocLG/kernelctl/actions/workflows/ci.yml/badge.svg)](https://github.com/MocLG/kernelctl/actions/workflows/ci.yml)

A single-binary CLI and interactive TUI for managing kernels and boot entries,
across whichever bootloader a Linux system actually uses. Runs on x86_64 and
ARM (ARMv7 / AArch64).

The problem it solves: every bootloader stores the same handful of facts in a
different format, so "make this kernel the default" is a different task on
every machine. kernelctl reads them all into one shape and exposes one set of
commands over the top.

```
$ kernelctl list
ID                   TITLE                  VERSION         BUILT       STATE
systemd-boot-113biz  Arch Linux             6.12.1-arch1-1  2026-08-08  [DEFAULT] [RUNNING]
systemd-boot-rf3p9v  Arch Linux (fallback)  6.12.1-arch1-1  2026-08-08  [RECOVERY]
systemd-boot-9ijd9j  Older kernel           6.9.3-arch1-1   2026-06-14
```

## Install

Prebuilt binaries are attached to every [release](../../releases). The **musl** builds are
statically linked and depend on nothing at all, so they run on any distribution regardless
of its glibc version:

```sh
curl -LO https://github.com/MocLG/kernelctl/releases/latest/download/kernelctl-<version>-x86_64-unknown-linux-musl.tar.gz
tar xzf kernelctl-*.tar.gz
sudo install -m755 kernelctl-*/kernelctl /usr/local/bin/kernelctl
kernelctl status
```

Builds are published for `x86_64` and `aarch64` (glibc and musl) and for `armv7` (musl).
Verify a download against the `SHA256SUMS` file attached to the release:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

Every push also uploads a binary to its CI run, which is useful for testing an unreleased
change; those expire after 14 days, so prefer a release for anything permanent.

## Supported bootloaders

| Bootloader | Read | Default | One-shot | Timeout | Cmdline |
|---|---|---|---|---|---|
| GRUB 2 | ✓ | ✓ | ✓ | ✓ | ✓ |
| GRUB Legacy (`menu.lst`) | ✓ | ✓ | | ✓ | ✓ |
| systemd-boot / gummiboot | ✓ | ✓ | ✓ | ✓ | ✓ |
| Limine (`.conf` and legacy `.cfg`) | ✓ | ✓ | | ✓ | ✓ |
| extlinux / U-Boot | ✓ | ✓ | | ✓ | ✓ |
| Syslinux / ISOLINUX / PXELINUX | ✓ | ✓ | | ✓ | ✓ |
| rEFInd | ✓ | ✓ | | ✓ | ✓ |
| LILO | ✓ | ✓ | ✓ (`lilo -R`) | ✓ | ✓ |
| EFI stub (firmware NVRAM) | ✓ | ✓ | ✓ | | |
| Barebox | ✓ | ✓ | | | |
| Unified Kernel Images | ✓ | | | | |

Blank cells are formats where the operation genuinely does not exist. kernelctl
reports that plainly rather than pretending — a UKI's command line is inside a
signed binary, and LILO has no "wait forever" timeout, so both are refused with
an explanation instead of a write that would not do what was asked.

Discovery probes every adapter and scores each one. The highest scorer is the
loader commands act on; the rest stay visible, so a machine with a GRUB install
left over from before a switch to systemd-boot behaves sensibly.

## Commands

```
kernelctl                          interactive interface
kernelctl status                   bootloader, arch, running kernel, boot space
kernelctl list [PATTERN]           entry table; --long adds paths and cmdlines
kernelctl loaders                  every bootloader found, and what each can do
kernelctl set-default <ID>         permanently change which entry boots
kernelctl set-next <ID>            boot an entry once; --clear cancels
kernelctl cmdline get <ID>         print an entry's kernel parameters
kernelctl cmdline set <ID> <ARGS>  replace them
kernelctl cmdline add <ID> <ARGS>  add, replacing any with the same key
kernelctl cmdline remove <ID> <K>  remove by name
kernelctl diff <ID> <ID>           compare two entries
kernelctl timeout [SECONDS]        read or set the menu timeout
kernelctl clean                    remove kernels no boot entry references
kernelctl backup                   archive configuration to a .tar.gz
kernelctl restore <FILE>           restore from an archive
kernelctl remove <ID>              delete a boot entry
kernelctl help                     full help, keybindings, architecture
```

`<ID>` accepts the id from `list`, an unambiguous prefix, a kernel version, or
part of a title. A pattern matching more than one entry is an error, not a
guess — booting the wrong entry is not something you find out about until the
machine reboots.

Global flags: `--boot-dir DIR` (repeatable, wins over auto-discovery),
`--loader NAME`, `--all`, `--json`, `--dry-run`, `-y/--yes`, `--color WHEN`,
`-v/--verbose`.

## Interactive interface

Running `kernelctl` with no arguments opens a terminal interface with a header
bar (hostname, running kernel, active bootloader, `[ROOT]`/`[USER]`), the entry
table, a details panel resolving the highlighted entry's kernel, initramfs and
command line, and a footer of keybindings.

| Key | Action |
|---|---|
| `↑`/`k`, `↓`/`j`, `g`/`G` | move |
| `Enter` / `d` | set as permanent default |
| `n` / `N` | boot once on next reboot / clear it |
| `e` | edit kernel command line |
| `t` | set menu timeout |
| `c` | clean up unused kernels |
| `b` | back up configuration |
| `/` | filter entries |
| `Tab` | cycle between primary and all bootloaders |
| `r` | re-read from disk |
| `?` / `h` | help overlay |
| `q` / `Esc` | quit |

Every action drives the same code the CLI does, so the two cannot diverge.

## Safety

- **Atomic writes.** Every config change copies the original to `.bak`, writes
  a temp file in the same directory, fsyncs it, renames it over the target,
  then fsyncs the directory. A crash leaves either the old file or the new one,
  never a truncated one. The exception is GRUB's `grubenv`, which is rewritten
  in place on purpose: it must stay exactly 1024 bytes and must not move on
  disk, or GRUB cannot update it from the boot menu.
- **Pre-flight validation.** Before changing what boots, the kernel and
  initramfs the entry names are verified to exist. This is the most valuable
  check in the program: a missing file is otherwise discovered at a firmware
  prompt with no way to undo it.
- **Read-only without root.** `status`, `list`, `diff` and `cmdline get` need
  no privileges. Writes are refused up front with the exact `sudo` line to run,
  rather than failing partway through.
- **Conservative cleanup.** A kernel is only removable when no entry from *any*
  detected loader references it, it is not running, not the newest installed,
  and its version parsed cleanly. If any config fails to parse, cleaning aborts:
  an incomplete picture is not a safe basis for deleting kernels.
- **No terminal, no guessing.** Confirmation prompts refuse to assume an answer
  when stdin is not a terminal; pass `--yes` deliberately.

## Building

```sh
cargo build --release      # target/release/kernelctl
cargo test
```

CI builds and tests natively on x86_64 and aarch64, cross-builds armv7, and checks the
declared MSRV, so a change that breaks any of those is caught before it lands.

Requires Rust 1.88 or newer (edition 2024). The release profile uses fat LTO, one codegen
unit, abort-on-panic and stripped symbols for a small, fast binary.

## Architecture

Four layers, each depending only on the one below it:

- **`sys/`** — everything touching the machine: uname, privileges, mounts and
  free space, external helpers, and the atomic write primitive.
- **`loaders/`** — one adapter per bootloader, each mapping its own format onto
  the normalized `BootEntry`. Adapters never talk to each other; discovery
  ranks them.
- **`commands/`** — the CLI verbs, working only with normalized entries, so no
  command needs to know which bootloader is installed.
- **`tui/`** — the interactive interface, driving the same command layer.

Dependencies are kept few and boring: `clap`, `ratatui`/`crossterm`, `rustix`
(no libc), `serde`, `tar`/`flate2`, `glob`, `unicode-width`. Date formatting,
hashing and terminal styling are implemented directly rather than pulling in
crates for a few dozen lines each.

## Licence

Dual-licensed: **GPL-3.0-only**, or a **commercial licence** if the GPL's obligations do
not suit you. See [`LICENSING.md`](LICENSING.md) for both options and the verified
third-party dependency audit, and [`CONTRIBUTING.md`](CONTRIBUTING.md) for the contributor
agreement that keeps the commercial option available.
