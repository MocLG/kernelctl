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
curl -fsSLO https://github.com/MocLG/kernelctl/releases/download/v1.0.0/kernelctl-v1.0.0-x86_64-unknown-linux-musl.tar.gz
tar xzf kernelctl-v1.0.0-x86_64-unknown-linux-musl.tar.gz
sudo install -m755 kernelctl-v1.0.0-x86_64-unknown-linux-musl/kernelctl /usr/local/bin/kernelctl
kernelctl status
```

Asset names carry the version, so there is no fixed "latest" URL to hard-code. To
always fetch the newest release, resolve the tag first:

```sh
tag=$(curl -fsSL https://api.github.com/repos/MocLG/kernelctl/releases/latest \
      | grep -m1 '"tag_name"' | cut -d'"' -f4)
curl -fsSLO "https://github.com/MocLG/kernelctl/releases/download/$tag/kernelctl-$tag-x86_64-unknown-linux-musl.tar.gz"
```

Builds are published for `x86_64` and `aarch64` (glibc and musl) and for `armv7` (musl);
substitute the target you want. Verify a download against the `SHA256SUMS` file attached
to the same release — note that a mistyped asset name returns an HTML error page rather
than failing outright, which is what a checksum mismatch on a fresh download usually means
(`curl -f` above turns that into a clean failure instead):

```sh
curl -fsSLO https://github.com/MocLG/kernelctl/releases/download/v1.0.0/SHA256SUMS
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

Global flags: `--boot-dir DIR`, `--loader NAME`, `--all`, `--json`, `--dry-run`,
`--apply`, `-y/--yes`, `--color WHEN`, `-v/--verbose`.

`--apply` matters on exactly two loaders. GRUB 2 takes its default from a
generated menu and LILO compiles its config into the boot sector, so on those
two a write is not yet a change — without `--apply` kernelctl says so and names
the command (`update-grub`, `lilo`); with it, kernelctl runs that command and
reports the result. Everywhere else the flag does nothing, because the file
kernelctl wrote is the file the bootloader reads.

`--boot-dir` **replaces** the automatic search rather than adding to it, so
`kernelctl --boot-dir /mnt/rescue/boot list` describes that tree alone — the running
system's `/boot`, its `/etc/default/grub` and its firmware boot entries are all
excluded. That is what makes it safe for inspecting a mounted image.

## Machine-readable output

`--json` prints an array of entries. The fields worth scripting against:

| Field | Meaning |
|---|---|
| `id` | stable identifier, also accepted by `set-default` and friends |
| `title`, `version`, `arch` | as shown in `list` |
| `kernel`, `initrds`, `devicetree`, `cmdline` | resolved absolute paths, and the command line |
| `state` | object of named booleans — `default`, `oneshot`, `running`, `broken`, `recovery`, `disabled`, `submenu`, `chainload`, `efi_stub`, `foreign_arch`, `unified` |
| `built` | build time as RFC 3339 UTC, or `null` |
| `loader`, `source`, `native_id` | which bootloader it came from, and how that loader names it |

```sh
kernelctl --json list | jq -r '.[] | select(.state.broken) | .id'
```

**Stability:** within a major version, fields are added but not removed or
retyped, so match on names and ignore what you do not recognise. `flags` (a raw
bitfield) and `build_time` (an internal timestamp rendering) are kept only for
compatibility with 1.0 — prefer `state` and `built`, which replace them.

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

Every action drives the same bootloader adapter the CLI does, and shares the
same rules for what may be made the default and for reporting a change a
bootloader has not picked up yet — so the two screens cannot disagree about
what is safe or about whether something took effect. `--apply` works here too;
without it, a change GRUB 2 or LILO has not applied is shown as a warning
rather than as done.

## Safety

- **Atomic writes.** Every config change copies the file's **previous contents**
  to `.bak`, writes a temp file in the same directory, fsyncs it, renames it
  over the target, then fsyncs the directory. A crash leaves either the old file
  or the new one, never a truncated one. The exception is GRUB's `grubenv`,
  which is rewritten in place on purpose: it must stay exactly 1024 bytes and
  must not move on disk, or GRUB cannot update it from the boot menu.
  `.bak` is one step of undo, not a history — it is replaced on every write, so
  after two edits it holds the state before the second, not the original. Use
  `kernelctl backup` for a copy that survives further changes; boot filesystems
  are small and often FAT, so kernelctl does not accumulate timestamped copies
  there on its own.
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
