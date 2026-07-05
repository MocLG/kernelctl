//! systemd-boot (and its gummiboot ancestor).
//!
//! Configuration is split three ways, and all three matter:
//!
//! - `<esp>/loader/loader.conf` holds the persistent default and timeout.
//! - `<esp>/loader/entries/*.conf` are the type-1 entries.
//! - `<esp>/EFI/Linux/*.efi` are type-2 entries: Unified Kernel Images that
//!   systemd-boot discovers by scanning, with no config file at all.
//!
//! The one-shot default is not a file: it lives in the `LoaderEntryOneShot`
//! EFI variable, which systemd-boot consumes and clears on the next boot.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::{self, WriteOutcome};

use super::{bls, efivars, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// systemd's variable holding the entry to boot exactly once.
const VAR_ONESHOT: &str = "LoaderEntryOneShot";
/// The entry systemd-boot recorded as the last/saved choice.
const VAR_DEFAULT: &str = "LoaderEntryDefault";

pub struct SystemdBoot {
    /// Partition holding `loader/`; paths inside entries are relative to it.
    esp: PathBuf,
    loader_conf: PathBuf,
    entries_dir: PathBuf,
    confidence: u8,
}

impl SystemdBoot {
    /// Look for a `loader` directory under any boot root.
    pub fn detect(roots: &BootRoots) -> Option<SystemdBoot> {
        for root in &roots.boot {
            let loader_dir = root.join("loader");
            let entries_dir = loader_dir.join("entries");
            let loader_conf = loader_dir.join("loader.conf");

            if !entries_dir.is_dir() && !loader_conf.is_file() {
                continue;
            }

            // The presence of the installed EFI binary is what distinguishes
            // "systemd-boot is installed here" from "some other loader reads
            // BLS entries from this directory".
            let installed = root.join("EFI/systemd").is_dir()
                || glob_first(&root.join("EFI/BOOT/BOOT*.EFI")).is_some_and(|p| {
                    // A generic fallback binary could belong to anything, so
                    // only count it when systemd's own copy sits beside it.
                    p.parent().is_some_and(|d| d.join("systemd-bootx64.efi").exists())
                });

            let confidence = if installed { 90 } else { 65 };

            return Some(SystemdBoot {
                esp: root.clone(),
                loader_conf,
                entries_dir,
                confidence,
            });
        }
        None
    }

    /// Type-2 entries: UKIs systemd-boot finds by scanning `EFI/Linux`.
    fn unified_entries(&self) -> Vec<BootEntry> {
        let dir = self.esp.join("EFI/Linux");
        let Ok(read) = std::fs::read_dir(&dir) else { return Vec::new() };

        let mut paths: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("efi")))
            .collect();
        paths.sort();

        paths
            .iter()
            .map(|path| {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let title = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();

                let mut entry =
                    BootEntry::new(LoaderKind::SystemdBoot, path, &file_name, title);
                // A UKI bundles kernel, initrd and cmdline into one binary, so
                // the image itself is the kernel and there is nothing else to
                // resolve.
                entry.kernel = Some(path.clone());
                entry.flags.insert(EntryFlags::UNIFIED | EntryFlags::EFI_STUB);
                entry.extra.insert("type".into(), "type-2 (unified kernel image)".into());
                entry
            })
            .collect()
    }

    /// The `default` pattern from loader.conf, resolved through `@saved`.
    fn default_pattern(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.loader_conf).ok()?;
        let value = bls::get_key(&text, "default")?;

        // `@saved` defers to whatever systemd-boot last recorded, so the real
        // answer is in the EFI variable rather than the file.
        if value.trim() == "@saved" {
            return efivars::read_string(VAR_DEFAULT, efivars::LOADER_GUID).ok().flatten();
        }
        Some(value)
    }
}

/// Does a systemd-boot `default`/entry pattern select this entry?
///
/// The value is an fnmatch-style glob against the entry filename, and both
/// `arch.conf` and `arch` are accepted spellings, so the comparison is tried
/// with and without the extension.
fn pattern_matches(pattern: &str, native_id: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let stem = native_id.strip_suffix(".conf").unwrap_or(native_id);

    if pattern == native_id || pattern == stem {
        return true;
    }
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(native_id) || p.matches(stem),
        // An invalid glob is a literal, and we already compared literally.
        Err(_) => false,
    }
}

fn glob_first(pattern: &Path) -> Option<PathBuf> {
    glob::glob(&pattern.to_string_lossy()).ok()?.flatten().next()
}

/// Render a timeout in loader.conf's vocabulary.
fn timeout_to_conf(timeout: Timeout) -> String {
    match timeout {
        // `menu-hidden` skips the menu entirely, which is what a zero timeout
        // means to every other loader.
        Timeout::Immediate => "menu-hidden".to_string(),
        Timeout::Seconds(n) => n.to_string(),
        Timeout::Indefinite => "menu-force".to_string(),
    }
}

fn timeout_from_conf(value: &str) -> Option<Timeout> {
    match value.trim().to_ascii_lowercase().as_str() {
        "menu-force" | "menu-disabled" => Some(Timeout::Indefinite),
        "menu-hidden" | "0" => Some(Timeout::Immediate),
        other => other.parse::<u32>().ok().map(Timeout::Seconds),
    }
}

impl Bootloader for SystemdBoot {
    fn kind(&self) -> LoaderKind {
        LoaderKind::SystemdBoot
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::SET_DEFAULT
            | Capabilities::SET_ONESHOT
            | Capabilities::TIMEOUT
            | Capabilities::EDIT_CMDLINE
            | Capabilities::REMOVE_ENTRY
    }

    fn confidence(&self) -> u8 {
        self.confidence
    }

    fn config_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.loader_conf.clone()];
        if let Ok(dir) = std::fs::read_dir(&self.entries_dir) {
            files.extend(
                dir.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("conf"))),
            );
        }
        files.retain(|p| p.exists());
        files
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let mut entries = bls::load_dir(&self.entries_dir, &self.esp, LoaderKind::SystemdBoot)?;
        entries.extend(self.unified_entries());

        if let Some(pattern) = self.default_pattern() {
            // A glob can match several entries; only the first counts, which
            // matches how systemd-boot resolves it.
            if let Some(e) = entries.iter_mut().find(|e| pattern_matches(&pattern, &e.native_id)) {
                e.flags.insert(EntryFlags::DEFAULT);
            }
        }

        if let Ok(Some(oneshot)) = efivars::read_string(VAR_ONESHOT, efivars::LOADER_GUID) {
            if let Some(e) = entries.iter_mut().find(|e| pattern_matches(&oneshot, &e.native_id)) {
                e.flags.insert(EntryFlags::ONESHOT);
            }
        }

        Ok(entries)
    }

    fn set_default(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("set-default", &self.loader_conf)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }

        // loader.conf may legitimately not exist yet; start from empty.
        let text = std::fs::read_to_string(&self.loader_conf).unwrap_or_default();
        let updated = bls::rewrite_key(&text, "default", &entry.native_id);
        Ok(vec![atomic::write_atomic(&self.loader_conf, updated.as_bytes())?])
    }

    fn set_oneshot(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next")?;
        if !efivars::available() {
            return Err(Error::validation(
                "EFI variables are not available, so a one-shot entry cannot be set; \
                 is this system booted via UEFI with efivarfs mounted?",
            ));
        }
        if ctx.dry_run {
            return Ok(Vec::new());
        }

        efivars::write_string(VAR_ONESHOT, efivars::LOADER_GUID, &entry.native_id)?;
        Ok(Vec::new())
    }

    fn clear_oneshot(&self, ctx: &Context) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next --clear")?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        efivars::remove(VAR_ONESHOT, efivars::LOADER_GUID)?;
        Ok(Vec::new())
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let Ok(text) = std::fs::read_to_string(&self.loader_conf) else { return Ok(None) };
        Ok(bls::get_key(&text, "timeout").as_deref().and_then(timeout_from_conf))
    }

    fn set_timeout(&self, ctx: &Context, timeout: Timeout) -> Result<Vec<WriteOutcome>> {
        ctx.preflight_write("timeout", &self.loader_conf)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&self.loader_conf).unwrap_or_default();
        let updated = bls::rewrite_key(&text, "timeout", &timeout_to_conf(timeout));
        Ok(vec![atomic::write_atomic(&self.loader_conf, updated.as_bytes())?])
    }

    fn set_cmdline(&self, ctx: &Context, entry: &BootEntry, cmdline: &str) -> Result<Vec<WriteOutcome>> {
        if entry.flags.contains(EntryFlags::UNIFIED) {
            return Err(Error::validation(
                "this is a unified kernel image: its command line is baked into the \
                 signed binary and must be changed by rebuilding the UKI",
            ));
        }
        ctx.preflight_write("cmdline set", &entry.source)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }

        let text = atomic::read_to_string(&entry.source)?;
        let updated = bls::rewrite_options(&text, cmdline);
        Ok(vec![atomic::write_atomic(&entry.source, updated.as_bytes())?])
    }

    fn remove_entry(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        if entry.flags.contains(EntryFlags::UNIFIED) {
            return Err(Error::validation(
                "removing a unified kernel image means deleting the .efi binary; \
                 use `kernelctl clean` so its kernel and modules go too",
            ));
        }
        ctx.preflight_write("remove", &entry.source)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        std::fs::remove_file(&entry.source).map_err(|e| Error::io(&entry.source, e))?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_plain_entry_names() {
        assert!(pattern_matches("arch.conf", "arch.conf"));
        // systemd-boot accepts the name with or without the extension.
        assert!(pattern_matches("arch", "arch.conf"));
        assert!(!pattern_matches("fedora.conf", "arch.conf"));
    }

    #[test]
    fn matches_glob_patterns() {
        // The auto-generated Arch entries are conventionally matched this way.
        assert!(pattern_matches("arch-*.conf", "arch-linux.conf"));
        assert!(pattern_matches("*.conf", "anything.conf"));
        assert!(!pattern_matches("fedora-*", "arch-linux.conf"));
    }

    #[test]
    fn empty_pattern_matches_nothing() {
        assert!(!pattern_matches("", "arch.conf"));
        assert!(!pattern_matches("   ", "arch.conf"));
    }

    #[test]
    fn round_trips_timeout_values() {
        assert_eq!(timeout_from_conf("5"), Some(Timeout::Seconds(5)));
        assert_eq!(timeout_from_conf("menu-force"), Some(Timeout::Indefinite));
        assert_eq!(timeout_from_conf("menu-hidden"), Some(Timeout::Immediate));
        assert_eq!(timeout_from_conf("0"), Some(Timeout::Immediate));
        assert_eq!(timeout_from_conf("nonsense"), None);

        assert_eq!(timeout_to_conf(Timeout::Seconds(10)), "10");
        assert_eq!(timeout_to_conf(Timeout::Indefinite), "menu-force");
        assert_eq!(timeout_to_conf(Timeout::Immediate), "menu-hidden");
    }

    #[test]
    fn timeout_conversion_survives_a_round_trip() {
        for t in [Timeout::Immediate, Timeout::Seconds(7), Timeout::Indefinite] {
            assert_eq!(timeout_from_conf(&timeout_to_conf(t)), Some(t));
        }
    }

    // ---- fixture-backed tests against a scratch ESP -----------------------

    use crate::loaders::testsupport::{fake_kernel, Fixture, TempTree};

    /// An ESP with two type-1 entries, one of them the default.
    fn esp(tag: &str) -> TempTree {
        let tree = TempTree::new(tag);
        tree.dir("EFI/systemd");
        tree.file("loader/loader.conf", "default arch.conf\ntimeout 4\neditor no\n");
        tree.file(
            "loader/entries/arch.conf",
            "title Arch Linux\nversion 6.11.5-arch1-1\nlinux /vmlinuz-linux\n\
             initrd /amd-ucode.img\ninitrd /initramfs-linux.img\noptions root=UUID=aaa rw quiet\n",
        );
        tree.file(
            "loader/entries/arch-fallback.conf",
            "title Arch Linux (fallback)\nversion 6.11.5-arch1-1\nlinux /vmlinuz-linux\n\
             initrd /initramfs-linux-fallback.img\noptions root=UUID=aaa rw\n",
        );
        fake_kernel(&tree, "vmlinuz-linux");
        fake_kernel(&tree, "amd-ucode.img");
        fake_kernel(&tree, "initramfs-linux.img");
        fake_kernel(&tree, "initramfs-linux-fallback.img");
        tree
    }

    #[test]
    fn detects_an_esp_with_a_loader_directory() {
        let tree = esp("sdboot-detect");
        let loader = SystemdBoot::detect(&tree.roots()).expect("should detect systemd-boot");
        assert_eq!(loader.esp, tree.root);
        // EFI/systemd is present, so this is an installed loader, not just a
        // directory of BLS entries some other loader reads.
        assert_eq!(loader.confidence, 90);
    }

    #[test]
    fn scores_bls_entries_without_the_binary_lower() {
        let tree = TempTree::new("sdboot-bls-only");
        tree.file("loader/entries/x.conf", "title X\nlinux /vmlinuz\n");
        let loader = SystemdBoot::detect(&tree.roots()).unwrap();
        assert_eq!(loader.confidence, 65);
    }

    #[test]
    fn does_not_detect_an_unrelated_tree() {
        let tree = TempTree::new("sdboot-absent");
        tree.file("grub/grub.cfg", "menuentry 'x' {}\n");
        assert!(SystemdBoot::detect(&tree.roots()).is_none());
    }

    #[test]
    fn parses_entries_and_marks_the_default() {
        let tree = esp("sdboot-entries");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();

        let entries = loader.entries(&fx.context()).unwrap();
        assert_eq!(entries.len(), 2);

        let arch = entries.iter().find(|e| e.native_id == "arch.conf").unwrap();
        assert_eq!(arch.title, "Arch Linux");
        assert!(arch.is_default());
        assert_eq!(arch.cmdline, "root=UUID=aaa rw quiet");
        // Entry paths are relative to the ESP, not the filesystem root.
        assert_eq!(arch.kernel.as_ref().unwrap(), &tree.path("vmlinuz-linux"));
        assert_eq!(arch.initrds.len(), 2);

        let fallback = entries.iter().find(|e| e.native_id == "arch-fallback.conf").unwrap();
        assert!(!fallback.is_default());
    }

    #[test]
    fn discovers_unified_kernel_images() {
        let tree = esp("sdboot-uki");
        fake_kernel(&tree, "EFI/Linux/arch-linux-6.12.1.efi");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();

        let entries = loader.entries(&fx.context()).unwrap();
        let uki = entries.iter().find(|e| e.native_id.ends_with(".efi")).expect("UKI listed");

        assert!(uki.flags.contains(EntryFlags::UNIFIED));
        assert!(uki.flags.contains(EntryFlags::EFI_STUB));
        assert_eq!(uki.title, "arch-linux-6.12.1");
    }

    #[test]
    fn resolves_a_glob_default_pattern() {
        let tree = esp("sdboot-glob");
        tree.file("loader/loader.conf", "default arch-*\ntimeout 4\n");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();

        let entries = loader.entries(&fx.context()).unwrap();
        let default = entries.iter().find(|e| e.is_default()).expect("a default must match");
        assert_eq!(default.native_id, "arch-fallback.conf");
    }

    #[test]
    fn reads_the_configured_timeout() {
        let tree = esp("sdboot-timeout-read");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        assert_eq!(loader.timeout(&fx.context()).unwrap(), Some(Timeout::Seconds(4)));
    }

    #[test]
    fn set_default_rewrites_loader_conf_and_keeps_other_keys() {
        let tree = esp("sdboot-setdefault");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let target = entries.iter().find(|e| e.native_id == "arch-fallback.conf").unwrap();

        let outcomes = loader.set_default(&fx.context(), target).unwrap();

        let conf = tree.read("loader/loader.conf");
        assert!(conf.contains("default arch-fallback.conf"));
        assert!(conf.contains("timeout 4"), "unrelated keys must survive");
        assert!(conf.contains("editor no"));
        assert!(!conf.contains("default arch.conf"));
        // The previous config must be preserved next to it.
        assert_eq!(outcomes.len(), 1);
        let bak = outcomes[0].backup.as_ref().expect("a .bak must be written");
        assert!(std::fs::read_to_string(bak).unwrap().contains("default arch.conf"));
    }

    #[test]
    fn set_timeout_writes_the_native_spelling() {
        let tree = esp("sdboot-settimeout");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();

        loader.set_timeout(&fx.context(), Timeout::Indefinite).unwrap();
        assert!(tree.read("loader/loader.conf").contains("timeout menu-force"));

        loader.set_timeout(&fx.context(), Timeout::Seconds(30)).unwrap();
        assert!(tree.read("loader/loader.conf").contains("timeout 30"));
    }

    #[test]
    fn set_cmdline_edits_only_the_options_line() {
        let tree = esp("sdboot-cmdline");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let arch = entries.iter().find(|e| e.native_id == "arch.conf").unwrap();

        loader.set_cmdline(&fx.context(), arch, "root=UUID=aaa rw debug").unwrap();

        let text = tree.read("loader/entries/arch.conf");
        assert!(text.contains("options root=UUID=aaa rw debug"));
        assert!(text.contains("title Arch Linux"));
        assert!(text.contains("initrd /amd-ucode.img"));
        assert!(!text.contains("quiet"));
    }

    #[test]
    fn cmdline_edit_is_refused_for_a_uki() {
        let tree = esp("sdboot-uki-cmdline");
        fake_kernel(&tree, "EFI/Linux/arch.efi");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let uki = entries.iter().find(|e| e.flags.contains(EntryFlags::UNIFIED)).unwrap();

        // The command line is inside the signed binary, so editing a config
        // file could not possibly take effect.
        let err = loader.set_cmdline(&fx.context(), uki, "quiet").unwrap_err();
        assert!(err.to_string().contains("unified kernel image"));
    }

    #[test]
    fn writes_are_refused_without_root() {
        let tree = esp("sdboot-noroot");
        let fx = Fixture::unprivileged(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();

        let err = loader.set_default(&fx.context(), &entries[0]).unwrap_err();
        assert!(matches!(err, Error::NeedsRoot { .. }));
        // The config must be untouched.
        assert!(tree.read("loader/loader.conf").contains("default arch.conf"));
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let tree = esp("sdboot-dryrun");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let target = entries.iter().find(|e| e.native_id == "arch-fallback.conf").unwrap();

        let mut ctx = fx.context();
        ctx.dry_run = true;
        loader.set_default(&ctx, target).unwrap();

        assert!(tree.read("loader/loader.conf").contains("default arch.conf"));
    }

    #[test]
    fn removes_an_entry_file() {
        let tree = esp("sdboot-remove");
        let fx = Fixture::rooted(tree.roots());
        let loader = SystemdBoot::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();
        let target = entries.iter().find(|e| e.native_id == "arch-fallback.conf").unwrap();

        loader.remove_entry(&fx.context(), target).unwrap();

        assert!(!tree.path("loader/entries/arch-fallback.conf").exists());
        assert!(tree.path("loader/entries/arch.conf").exists());
    }

    #[test]
    fn lists_config_files_for_backup() {
        let tree = esp("sdboot-configs");
        let loader = SystemdBoot::detect(&tree.roots()).unwrap();
        let files = loader.config_files();
        assert!(files.contains(&tree.path("loader/loader.conf")));
        assert!(files.contains(&tree.path("loader/entries/arch.conf")));
        assert_eq!(files.len(), 3);
    }
}
