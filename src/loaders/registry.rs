/*
 * kernelctl — unified kernel and boot configuration management across Linux bootloaders.
 * Copyright (C) 2026 Luka Gejak
 *
 * This program is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License, version 3, as published
 * by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 * more details.
 *
 * You should have received a copy of the GNU General Public License along
 * with this program. If not, see <https://www.gnu.org/licenses/>.
 *
 * Alternatively, this file is available under a commercial licence that lifts
 * the obligations of the GPL. Enquiries: lukagejak5@gmail.com
 */
//! Bootloader discovery.
//!
//! Machines routinely have more than one bootloader's configuration on disk:
//! a GRUB install left behind after switching to systemd-boot, an ESP with
//! both, a leftover `syslinux.cfg` on a USB stick. Rather than guessing once
//! and possibly wrongly, every adapter is probed, each reports how confident
//! it is, and the highest scorer becomes the primary loader while the rest
//! stay visible under `--all`.
//!
//! Probing is cheap - a handful of `stat` calls per adapter - so this runs on
//! every invocation rather than being cached.

use crate::error::{Error, Result};
use crate::model::{BootEntry, LoaderKind};
use crate::sys::Host;

use super::{
    annotate, barebox::Barebox, efistub::EfiStub, grub2::Grub2, grub_legacy::GrubLegacy,
    lilo::Lilo, limine::Limine, refind::Refind, scan::BootRoots, sort_entries,
    syslinux::Syslinux, systemd_boot::SystemdBoot, uki::Uki, Bootloader, Context,
};

/// Every adapter that was found on this system, best first.
pub struct Discovery {
    pub loaders: Vec<Box<dyn Bootloader>>,
}

/// Probe every adapter and rank what was found.
///
/// Detection is deliberately independent per adapter: none of them consult the
/// others, so adding a new one cannot change how the existing ones behave.
pub fn discover(roots: &BootRoots) -> Discovery {
    let mut loaders: Vec<Box<dyn Bootloader>> = Vec::new();

    // Each probe is wrapped so that one adapter tripping over an unreadable
    // directory cannot hide every other loader on the system.
    macro_rules! probe {
        ($ty:ty) => {
            if let Some(found) = <$ty>::detect(roots) {
                loaders.push(Box::new(found));
            }
        };
    }

    probe!(SystemdBoot);
    probe!(Grub2);
    probe!(Limine);
    probe!(Refind);
    probe!(Syslinux);
    probe!(Barebox);
    probe!(GrubLegacy);
    probe!(Lilo);
    probe!(Uki);
    probe!(EfiStub);

    // Highest confidence first; ties break on the enum order so the result is
    // stable across runs rather than depending on probe order.
    loaders.sort_by(|a, b| {
        b.confidence().cmp(&a.confidence()).then_with(|| a.kind().cmp(&b.kind()))
    });

    Discovery { loaders }
}

impl Discovery {
    /// The loader treated as active for mutating commands.
    pub fn primary(&self) -> Result<&dyn Bootloader> {
        self.loaders.first().map(|b| b.as_ref()).ok_or(Error::NoBootloader)
    }

    pub fn is_empty(&self) -> bool {
        self.loaders.is_empty()
    }

    /// Find a detected loader by name, for `--loader grub2`.
    pub fn by_kind(&self, kind: LoaderKind) -> Option<&dyn Bootloader> {
        self.loaders.iter().find(|l| l.kind() == kind).map(|b| b.as_ref())
    }

    /// Kinds that were detected, in ranked order.
    pub fn kinds(&self) -> Vec<LoaderKind> {
        self.loaders.iter().map(|l| l.kind()).collect()
    }

    /// Entries from the primary loader, annotated and sorted.
    pub fn entries(&self, ctx: &Context) -> Result<Vec<BootEntry>> {
        let loader = self.primary()?;
        let mut entries = loader.entries(ctx)?;
        finish(&mut entries, ctx.host);
        Ok(entries)
    }

    /// Entries from every detected loader.
    ///
    /// A loader that fails to parse is skipped rather than aborting the whole
    /// listing: one corrupt leftover config should not make the working
    /// loader's entries unavailable.
    pub fn all_entries(&self, ctx: &Context) -> (Vec<BootEntry>, Vec<(LoaderKind, Error)>) {
        let mut entries = Vec::new();
        let mut errors = Vec::new();

        for loader in &self.loaders {
            match loader.entries(ctx) {
                Ok(mut found) => entries.append(&mut found),
                Err(e) if e.is_not_found() => {}
                Err(e) => errors.push((loader.kind(), e)),
            }
        }

        finish(&mut entries, ctx.host);
        (entries, errors)
    }
}

/// Annotate and sort a freshly parsed set of entries.
fn finish(entries: &mut Vec<BootEntry>, host: &Host) {
    annotate(entries, host);
    sort_entries(entries);
}

/// Resolve a user-supplied pattern to exactly one entry.
///
/// Ambiguity is an error rather than a silent pick: `set-default` on the wrong
/// entry is not something the user finds out about until the machine reboots.
pub fn resolve<'a>(entries: &'a [BootEntry], pattern: &str) -> Result<&'a BootEntry> {
    let mut matches: Vec<(&BootEntry, u8)> = entries
        .iter()
        .filter_map(|e| e.match_rank(pattern).map(|rank| (e, rank)))
        .collect();

    if matches.is_empty() {
        return Err(Error::EntryNotFound { pattern: pattern.to_string() });
    }

    matches.sort_by_key(|(_, rank)| *rank);
    let best = matches[0].1;
    let tied: Vec<&BootEntry> =
        matches.iter().filter(|(_, r)| *r == best).map(|(e, _)| *e).collect();

    // An exact-quality match that is unique wins even if fuzzier matches exist.
    if tied.len() == 1 {
        return Ok(tied[0]);
    }

    Err(Error::AmbiguousEntry {
        pattern: pattern.to_string(),
        matches: tied.iter().take(6).map(|e| format!("{} ({})", e.id, e.title)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};
    use crate::model::{EntryFlags, LoaderKind};

    #[test]
    fn finds_nothing_on_an_empty_tree() {
        let tree = TempTree::new("registry-empty");
        let d = discover(&tree.roots());
        assert!(d.is_empty());
        // `dyn Bootloader` is not Debug, so match on the error directly
        // rather than unwrapping the Result.
        assert!(matches!(d.primary().err(), Some(Error::NoBootloader)));
    }

    #[test]
    fn a_scoped_scan_reports_no_host_global_loaders() {
        // EFI NVRAM, Barebox's /env and /etc/lilo.conf belong to the running
        // machine. A scan aimed at another tree must not report them as if
        // they had been found there - this is what made CI fail on a runner
        // whose firmware exposes Boot#### entries.
        let tree = TempTree::new("registry-scoped");
        tree.file("grub/grub.cfg", "menuentry 'Linux' {\n\tlinux /vmlinuz\n}\n");
        fake_kernel(&tree, "vmlinuz");

        let found = discover(&tree.roots()).kinds();
        for host_global in
            [LoaderKind::EfiStub, LoaderKind::Barebox, LoaderKind::Lilo]
        {
            assert!(!found.contains(&host_global), "{host_global} leaked into a scoped scan");
        }
        assert_eq!(found, vec![LoaderKind::Grub2]);
    }

    #[test]
    fn ranks_the_more_confident_loader_first() {
        let tree = TempTree::new("registry-rank");
        // A full systemd-boot install...
        tree.dir("EFI/systemd");
        tree.file("loader/loader.conf", "default arch.conf\ntimeout 3\n");
        tree.file("loader/entries/arch.conf", "title Arch\nlinux /vmlinuz-linux\n");
        // ...beside a leftover syslinux config from an old USB install.
        tree.file("syslinux/syslinux.cfg", "DEFAULT linux\nLABEL linux\n    KERNEL /vmlinuz\n");
        fake_kernel(&tree, "vmlinuz-linux");

        let d = discover(&tree.roots());
        assert_eq!(d.primary().unwrap().kind(), LoaderKind::SystemdBoot);
        // The leftover is still visible rather than hidden.
        assert!(d.kinds().contains(&LoaderKind::Syslinux));
    }

    #[test]
    fn grub_outranks_a_bare_bls_entries_directory() {
        let tree = TempTree::new("registry-grub");
        tree.file("grub/grub.cfg", "menuentry 'Linux' {\n\tlinux /vmlinuz\n}\n");
        // GRUB's blscfg module reads these too, so their presence alone must
        // not outrank a real grub.cfg.
        tree.file("loader/entries/x.conf", "title X\nlinux /vmlinuz\n");
        fake_kernel(&tree, "vmlinuz");

        assert_eq!(discover(&tree.roots()).primary().unwrap().kind(), LoaderKind::Grub2);
    }

    #[test]
    fn collects_entries_from_every_loader() {
        let tree = TempTree::new("registry-all");
        tree.file("loader/loader.conf", "default arch.conf\n");
        tree.file("loader/entries/arch.conf", "title Arch\nlinux /vmlinuz-linux\n");
        tree.file("extlinux/extlinux.conf", "DEFAULT l\nLABEL l\n    LINUX /Image\n");
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "Image");

        let fx = Fixture::rooted(tree.roots());
        let d = discover(&fx.roots);
        let (entries, errors) = d.all_entries(&fx.context());

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert!(entries.iter().any(|e| e.loader == LoaderKind::SystemdBoot));
        assert!(entries.iter().any(|e| e.loader == LoaderKind::Extlinux));
    }

    #[test]
    fn entries_come_back_annotated_and_sorted() {
        let tree = TempTree::new("registry-annotate");
        tree.dir("EFI/systemd");
        tree.file("loader/loader.conf", "default b.conf\n");
        tree.file("loader/entries/a.conf", "title A\nversion 6.9.0\nlinux /vmlinuz-a\n");
        tree.file("loader/entries/b.conf", "title B\nversion 6.1.0\nlinux /vmlinuz-b\n");
        fake_kernel(&tree, "vmlinuz-a");
        fake_kernel(&tree, "vmlinuz-b");

        let fx = Fixture::rooted(tree.roots());
        let entries = discover(&fx.roots).entries(&fx.context()).unwrap();

        // The default sorts first even though its version is older.
        assert_eq!(entries[0].title, "B");
        assert!(entries[0].is_default());
        // Annotation ran: both kernels exist, so neither is broken.
        assert!(entries.iter().all(|e| !e.flags.contains(EntryFlags::BROKEN)));
    }

    // ---- pattern resolution ---------------------------------------------

    fn sample_entries() -> Vec<BootEntry> {
        let mut a = BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "id-a", "Arch Linux");
        a.version = crate::model::KernelVersion::parse("6.11.0");
        let mut b =
            BootEntry::new(LoaderKind::Grub2, "/boot/grub/grub.cfg", "id-b", "Arch Linux (fallback)");
        b.version = crate::model::KernelVersion::parse("6.11.0");
        vec![a, b]
    }

    #[test]
    fn resolves_an_exact_id() {
        let entries = sample_entries();
        let target = entries[1].id.clone();
        assert_eq!(resolve(&entries, &target).unwrap().title, "Arch Linux (fallback)");
    }

    #[test]
    fn resolves_by_native_id() {
        let entries = sample_entries();
        assert_eq!(resolve(&entries, "id-b").unwrap().title, "Arch Linux (fallback)");
    }

    #[test]
    fn reports_no_match() {
        let entries = sample_entries();
        assert!(matches!(
            resolve(&entries, "fedora").unwrap_err(),
            Error::EntryNotFound { .. }
        ));
    }

    #[test]
    fn ambiguity_is_an_error_not_a_guess() {
        let entries = sample_entries();
        // Both titles contain "Arch Linux", and booting the wrong one is not
        // discovered until the machine reboots.
        let err = resolve(&entries, "Arch").unwrap_err();
        match err {
            Error::AmbiguousEntry { matches, .. } => assert_eq!(matches.len(), 2),
            other => panic!("expected AmbiguousEntry, got {other}"),
        }
    }

    #[test]
    fn an_exact_title_beats_a_substring_match() {
        let entries = sample_entries();
        // "Arch Linux" is also a substring of the fallback entry's title, but
        // the exact match is unambiguous and wins.
        assert_eq!(resolve(&entries, "Arch Linux").unwrap().title, "Arch Linux");
    }
}
