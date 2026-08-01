# Contributing to kernelctl

Thanks for your interest. Bug reports, ideas and patches are all welcome.

Before sending code, please read the [Contributor License Agreement](#contributor-license-agreement)
below — **it requires you to assign copyright in your contribution to the project owner.**
That is a stronger requirement than most open-source projects impose, and it is stated up
front so nobody is surprised after the work is done. If you are not willing to assign
copyright, please don't send a patch; open an issue describing the change instead, and it
can be implemented independently.

---

## Getting set up

```bash
cargo build
cargo test
```

Rust 1.85 or newer is required (the crate is on edition 2024). See [`README.md`](README.md)
for the architecture and the reasoning behind the main design decisions.

kernelctl can be pointed at a fake boot tree instead of the real one, which is how to
exercise it without a bootloader present:

```bash
cargo run -- --boot-dir /path/to/fake-esp list
```

## Before you open a pull request

- **`cargo test` must pass and `cargo build` must be warning-free.** The binary currently
  builds with zero warnings; keep it that way.
- **Add tests for parser changes.** Every bootloader adapter is tested against config
  fixtures in its own `#[cfg(test)]` module, and mutations are tested against scratch
  directory trees built by `loaders::testsupport`. If you touch a parser, add a case that
  would have failed before your change.
- **Never run destructive commands against the host's real bootloader while developing.**
  Use `--boot-dir` with a scratch tree, or `--dry-run`. The test suite does this
  throughout and must continue to; a test that writes to `/boot` will be rejected.
- **Respect the safety invariants.** Config writes go through `sys::atomic`, which keeps a
  `.bak` and renames atomically; anything that changes what the machine boots must first
  verify the kernel and initramfs exist on disk. If you add an adapter, wire it into
  `loaders::registry` with an honest confidence score and declare only the capabilities it
  genuinely has — advertising one that does not work is worse than not having it.
- **Keep the licence header.** New source files carry the same
  `SPDX-License-Identifier: GPL-3.0-only` tag as the existing ones; don't remove them from
  existing files.
- **Match the surrounding style.** Comments here explain *why* a decision was made,
  especially where the obvious approach is wrong — see the notes on GRUB's fixed-size
  environment block, syslinux's inverted `TIMEOUT 0`, and why efivarfs needs a single
  write. Keep that standard.

## Adding a bootloader

Implement the `Bootloader` trait in `src/loaders/`, add a `detect()` that returns `None`
cheaply when the loader is absent, and register it in `src/loaders/registry.rs`. Parsing
is required; every mutating operation defaults to reporting itself unsupported, so a
read-only adapter is a few dozen lines. Include fixture tests using a real config file
from that bootloader's documentation, not a synthetic one.

## Reporting bugs

Include the distribution, the bootloader, `uname -m` and `uname -r`, what you expected and
what happened. If it involves parsing, attach the config file concerned (redact UUIDs and
hostnames if you prefer) so the exact input can be reproduced.

---

## Contributor License Agreement

By submitting a contribution to this project, you agree to the following. This applies to
every contribution you make, whether by pull request, patch, email, issue attachment or
any other means.

**1. Definitions.** "Owner" means Luka Gejak, the copyright holder of this project.
"You" means the individual or legal entity making the contribution. "Contribution" means
any original work of authorship you submit for inclusion in the project, including code,
documentation, tests, assets, configuration and modifications to existing files.

**2. Assignment of copyright.** You hereby assign to the Owner, exclusively, irrevocably
and worldwide, all right, title and interest in and to your Contribution, including all
copyright and all other intellectual property rights in it, for the full term of those
rights including any renewals, reversions and extensions. You agree to sign any further
documents the Owner reasonably requests in order to give effect to, record, or perfect
this assignment.

**3. Fallback licence.** Some jurisdictions do not permit the outright transfer of certain
authors' rights. To the extent that the assignment in section 2 is ineffective,
unenforceable or not permitted under the law applying to you, you instead grant the Owner
an exclusive, worldwide, royalty-free, perpetual, irrevocable licence — with the
unrestricted right to sub-license through multiple tiers — to use, reproduce, modify,
adapt, publish, translate, create derivative works from, distribute, publicly perform and
publicly display your Contribution, in any medium and by any means now known or later
devised, and to license it to others under any terms, including proprietary and commercial
terms.

This fallback exists because the law of some jurisdictions — including Germany and
France — does not permit an author to transfer copyright outright. In those jurisdictions
section 2 has no effect and this section governs instead.

**4. Patents.** You grant the Owner and all recipients of the software a perpetual,
worldwide, non-exclusive, royalty-free, irrevocable licence under any patent claims you
own or control that are necessarily infringed by your Contribution alone or by its
combination with the project, to make, have made, use, offer to sell, sell, import and
otherwise transfer the work.

**5. Moral rights.** To the fullest extent permitted by the law applying to you, you waive
and agree not to assert any moral rights in your Contribution against the Owner or anyone
receiving the software from the Owner. Where such rights cannot be waived, you agree not
to assert them in a way that would prevent the Owner from exercising the rights granted
here.

**6. Your representations.** You represent that:

- the Contribution is your original work, or you have the right to submit it under these
  terms;
- you are legally entitled to grant the rights above, and doing so does not breach any
  agreement with an employer, client or other party — if your employer has rights in work
  you produce, you have obtained their permission or they have waived those rights;
- to your knowledge the Contribution does not infringe anyone's intellectual property
  rights;
- any third-party material in the Contribution is clearly identified, along with its
  licence and any restrictions attached to it.

**7. Rights retained by you.** Nothing here stops you using your own Contribution for your
own purposes. You keep the right to use, publish and license the code you wrote elsewhere,
independently of this project. The assignment gives the Owner the ability to license the
project as a whole; it is not intended to take away your ability to reuse your own work.

**8. Licensing of the project.** You understand and agree that the Owner may license the
project, including your Contribution, under any terms whatsoever — including the GPL v3,
proprietary commercial licences, or both, as described in [`LICENSING.md`](LICENSING.md) —
and is under no obligation to keep the project free software, to use your Contribution, or
to pay you anything for it.

**9. No warranty.** Unless required by law or agreed in writing, you provide your
Contribution "as is", without warranties or conditions of any kind, express or implied.

**10. Entire agreement.** This is the whole agreement between you and the Owner concerning
contributions, and it supersedes any earlier understanding on the subject.

### How to accept

Add a `Signed-off-by` line to each commit, using your real name and an address you can be
reached at:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it for you. Including that line in a commit you submit means you
accept this agreement for that contribution. If you are contributing on behalf of an
employer, say so in the pull request and confirm you have authority to bind them.
