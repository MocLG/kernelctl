# Licensing

kernelctl is **dual-licensed**. You may use it under either:

1. the **GNU General Public License, version 3** (see [`LICENSE`](LICENSE)) — free of
   charge, with the obligations that licence imposes; or
2. a **commercial licence** from the copyright holder, which removes those obligations.

Copyright © 2026 Luka Gejak. All rights reserved.

---

## Option 1 — GPL v3 (free)

You may use, study, modify and redistribute this software under the GPL v3. In return,
the licence requires broadly that:

- **Derivative works must also be GPL v3.** If you distribute a modified version, or a
  larger work that incorporates this code, that whole work must be released under the
  GPL v3.
- **You must provide source.** Anyone you distribute a binary to is entitled to the
  corresponding source code, including your modifications.
- **Notices must be preserved.** Copyright and licence notices stay intact.
- **No warranty.** The software is provided as-is; see sections 15–17 of the licence.

[`LICENSE`](LICENSE) is the authoritative text and is reproduced verbatim. Where anything
in this file appears to conflict with it, the licence text governs.

## Option 2 — Commercial licence (paid)

If the GPL's obligations do not work for you, a commercial licence is available. It is
intended for people who want to use this code **without** having to release their own
source.

You likely want a commercial licence if you intend to:

- ship a **closed-source** product that incorporates this code, in whole or in part —
  bundling kernelctl into a proprietary appliance image or device firmware is the usual
  case here;
- distribute a modified version without publishing your modifications;
- redistribute under terms of your own choosing, including sub-licensing to your customers;
- distribute through a channel whose terms are difficult to reconcile with the GPL;
- obtain a warranty, an indemnity, or a support commitment, none of which the GPL provides.

**To enquire, contact Luka Gejak at [lukagejak5@gmail.com](mailto:lukagejak5@gmail.com).**
Please describe how you intend to use the software so the terms and price can be scoped.

### Why this is possible

Dual licensing only works if one party owns all the rights. Every contribution to this
project is assigned to the copyright holder under the agreement in
[`CONTRIBUTING.md`](CONTRIBUTING.md), which keeps the ownership undivided and makes the
commercial option available. This is the same arrangement used by projects such as Qt and
MySQL.

---

## Third-party dependencies

kernelctl links against the crates below. **All are permissive**, which matters twice
over: permissive terms are compatible with distribution under the GPL v3, and — because
none of them is copyleft — they can equally be shipped under the commercial option. A
copyleft dependency would break the second option even though it left the first intact.

GPL v3 was chosen rather than GPL v2 because Apache 2.0 is incompatible with GPL v2 but
explicitly compatible with GPL v3 (one-way: Apache-licensed code may be incorporated into
a GPL v3 work). Several dependencies here are Apache-2.0-or-MIT, so v3 is the floor.

Direct dependencies:

| Dependency | Licence | Note |
|---|---|---|
| `clap` | MIT OR Apache-2.0 | command-line parsing |
| `ratatui` | MIT | terminal interface |
| `crossterm` | MIT | terminal backend |
| `rustix` | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | syscalls without a libc crate |
| `serde`, `serde_json` | MIT OR Apache-2.0 | `--json` output |
| `tar` | MIT OR Apache-2.0 | backup archives |
| `flate2` | MIT OR Apache-2.0 | gzip, pure-Rust backend |
| `glob` | MIT OR Apache-2.0 | loader default-entry patterns |
| `unicode-width` | MIT OR Apache-2.0 | table column alignment |

Across the full transitive graph (101 crates) the licences resolve to: MIT OR Apache-2.0
and its spelling variants (73), MIT (17), Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR
MIT (5), MIT OR Zlib OR Apache-2.0 (1), and one each of 0BSD, Zlib, BSL-1.0, Unlicense and
Unicode-3.0 — all permitting incorporation into a GPL v3 work.

Licences were verified with `cargo metadata` against the exact versions in
[`Cargo.lock`](Cargo.lock), not from recollection. Re-check if you change or add a
dependency — a new copyleft dependency would break the **commercial** option, and a
GPL-incompatible one (proprietary, or SSPL/BUSL-style source-available terms) would break
both.

## Relationship to the software it manages

kernelctl reads and edits the configuration files of bootloaders such as GRUB, systemd-boot
and Limine. It **does not link against, embed, or derive from** any of their code, and
their licences therefore do not reach this project. Editing a GPL-licensed program's
configuration file no more creates a derivative work of it than editing an `/etc` file
creates a derivative work of the daemon that reads it.

Boot configuration files on a user's machine, and the kernels they point at, are the work
of their respective authors and are not covered by this project's licence.

*Linux* is a registered trademark of Linus Torvalds. kernelctl is an independent project,
not affiliated with or endorsed by the Linux Foundation, kernel.org, or the authors of any
bootloader it supports.
