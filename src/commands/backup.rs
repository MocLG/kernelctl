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
//! `kernelctl backup` and `kernelctl restore`.
//!
//! The archive holds every config file the detected loaders own, plus a
//! manifest describing the machine it came from. Files are stored under their
//! full absolute path (with the leading slash removed, as tar requires), so a
//! restore knows exactly where each one belongs without guessing.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sys::atomic;
use crate::ui::style;
use crate::util::time;

use super::{success, App};

/// Name of the manifest inside the archive.
const MANIFEST: &str = "kernelctl-manifest.json";

/// Describes the system a backup was taken from.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub created: String,
    pub hostname: String,
    pub kernel: String,
    pub architecture: String,
    pub bootloaders: Vec<String>,
    pub files: Vec<String>,
}

/// Files worth archiving beyond what the loaders declare.
const EXTRA_PATHS: &[&str] = &["/etc/default/grub", "/etc/kernel/cmdline", "/etc/lilo.conf"];

pub fn backup(app: &App, output: Option<&Path>) -> Result<()> {
    if app.discovery.is_empty() {
        return Err(Error::NoBootloader);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for loader in &app.discovery.loaders {
        files.extend(loader.config_files());
    }
    for extra in EXTRA_PATHS {
        let p = PathBuf::from(extra);
        if p.is_file() {
            files.push(p);
        }
    }

    // Canonicalize before deduplicating: two loaders can name the same file
    // through different paths (a symlinked /boot/efi, say).
    files = dedupe(files);
    files.retain(|p| p.is_file());

    if files.is_empty() {
        return Err(Error::validation(
            "found no readable bootloader configuration to back up",
        ));
    }

    let stamp = time::Utc::now().format_stamp();
    let path = output.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from(format!("kernelctl-backup-{}-{stamp}.tar.gz", app.host.hostname))
    });

    let manifest = Manifest {
        version: 1,
        created: time::Utc::now().format_minutes(),
        hostname: app.host.hostname.clone(),
        kernel: app.host.kernel_release.clone(),
        architecture: app.host.arch.to_string(),
        bootloaders: app.discovery.kinds().iter().map(|k| k.to_string()).collect(),
        files: files.iter().map(|p| p.display().to_string()).collect(),
    };

    if app.args.dry_run {
        super::dry_run_notice(&format!("write {} containing:", path.display()));
        for f in &files {
            println!("  {}", f.display());
        }
        return Ok(());
    }

    write_archive(&path, &files, &manifest)?;

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    success(&format!(
        "backed up {} file{} to {} ({})",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        style::bold(&path.display().to_string()),
        time::format_bytes(size)
    ));

    if app.args.verbose {
        for f in &files {
            println!("  {}", style::dim(&f.display().to_string()));
        }
    }
    // NVRAM-based entries have no file to archive, so say so rather than
    // letting the backup look more complete than it is.
    if app.discovery.kinds().contains(&crate::model::LoaderKind::EfiStub) {
        super::note_line(
            "EFI NVRAM boot entries are not files and are not included; \
             record them separately with `efibootmgr -v`",
        );
    }

    Ok(())
}

/// Remove duplicates, following symlinks so the same file is not stored twice.
fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for p in paths {
        let key = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p);
        }
    }
    out
}

fn write_archive(path: &Path, files: &[PathBuf], manifest: &Manifest) -> Result<()> {
    let file = std::fs::File::create(path).map_err(|e| Error::io(path, e))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);

    let manifest_json = serde_json::to_vec_pretty(manifest)
        .map_err(|e| Error::other(format!("could not build the manifest: {e}")))?;

    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(time::unix_secs(std::time::SystemTime::now()).max(0) as u64);
    header.set_cksum();
    tar.append_data(&mut header, MANIFEST, manifest_json.as_slice())
        .map_err(|e| Error::io(path, e))?;

    for source in files {
        // tar rejects absolute paths, so the leading slash is dropped and
        // restored on the way back out.
        let archived = source.strip_prefix("/").unwrap_or(source);
        tar.append_path_with_name(source, archived).map_err(|e| Error::io(source, e))?;
    }

    // finish() flushes the tar; into_inner() then flushes the gzip stream.
    // Skipping either truncates the archive.
    let encoder = tar.into_inner().map_err(|e| Error::io(path, e))?;
    encoder.finish().map_err(|e| Error::io(path, e))?;
    Ok(())
}

/// Open an archive and read its manifest and file list.
fn read_archive(path: &Path) -> Result<(Option<Manifest>, Vec<(String, Vec<u8>)>)> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    let mut manifest = None;
    let mut files = Vec::new();

    for entry in tar.entries().map_err(|e| Error::io(path, e))? {
        let mut entry = entry.map_err(|e| Error::io(path, e))?;
        let name = entry.path().map_err(|e| Error::io(path, e))?.display().to_string();

        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).map_err(|e| Error::io(path, e))?;

        if name == MANIFEST {
            manifest = serde_json::from_slice(&contents).ok();
        } else {
            files.push((name, contents));
        }
    }

    Ok((manifest, files))
}

pub fn restore(app: &App, archive: &Path, list_only: bool) -> Result<()> {
    let (manifest, files) = read_archive(archive)?;

    if let Some(m) = &manifest {
        println!("{}", style::heading("Backup"));
        println!("  {}  {}", style::label("created "), m.created);
        println!("  {}  {}", style::label("host    "), m.hostname);
        println!("  {}  {}", style::label("kernel  "), m.kernel);
        println!("  {}  {}", style::label("loaders "), m.bootloaders.join(", "));
        println!();

        // Restoring another machine's boot config is occasionally deliberate
        // and usually a mistake, so it is flagged either way.
        if m.hostname != app.host.hostname {
            super::warn(&format!(
                "this backup came from '{}' but this machine is '{}'",
                m.hostname, app.host.hostname
            ));
        }
        if m.architecture != app.host.arch.to_string() {
            super::warn(&format!(
                "this backup came from {} but this machine is {}",
                m.architecture, app.host.arch
            ));
        }
    } else {
        super::warn("this archive has no kernelctl manifest; restoring it anyway");
    }

    if list_only {
        println!("{}", style::heading("Files"));
        for (name, contents) in &files {
            println!("  /{name}  {}", style::dim(&time::format_bytes(contents.len() as u64)));
        }
        return Ok(());
    }

    app.privileges.require("restore")?;

    println!("{}", style::heading("Would restore"));
    for (name, _) in &files {
        println!("  /{name}");
    }
    println!();

    if !app.confirm(&format!(
        "Overwrite {} boot configuration file{}?",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    ))? {
        println!("cancelled");
        return Ok(());
    }

    if app.args.dry_run {
        super::dry_run_notice(&format!("restore {} files", files.len()));
        return Ok(());
    }

    let mut restored = 0usize;
    for (name, contents) in &files {
        let target = PathBuf::from("/").join(name);

        let Some(parent) = target.parent() else { continue };
        if !parent.is_dir() {
            super::warn(&format!(
                "skipping {}: {} does not exist on this system",
                target.display(),
                parent.display()
            ));
            continue;
        }

        // Each restored file gets a .bak of what was there, so a restore is
        // itself undoable.
        atomic::write_atomic(&target, contents)?;
        restored += 1;
        if app.args.verbose {
            println!("  {} {}", style::dim("restored"), target.display());
        }
    }

    success(&format!("restored {restored} file{}", if restored == 1 { "" } else { "s" }));
    super::note_line("the previous contents of each file were saved alongside it as .bak");

    if let Some(loader) = app.discovery.loaders.first() {
        app.print_note(loader.as_ref());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::TempTree;

    #[test]
    fn archive_round_trips_files_and_manifest() {
        let tree = TempTree::new("backup-roundtrip");
        let a = tree.file("loader/loader.conf", "default arch.conf\ntimeout 4\n");
        let b = tree.file("loader/entries/arch.conf", "title Arch\nlinux /vmlinuz\n");
        let archive = tree.path("backup.tar.gz");

        let manifest = Manifest {
            version: 1,
            created: "2026-08-08 12:00".into(),
            hostname: "testhost".into(),
            kernel: "6.11.0".into(),
            architecture: "aarch64".into(),
            bootloaders: vec!["systemd-boot".into()],
            files: vec![a.display().to_string(), b.display().to_string()],
        };

        write_archive(&archive, &[a.clone(), b.clone()], &manifest).unwrap();
        assert!(archive.exists());

        let (read_manifest, files) = read_archive(&archive).unwrap();

        let m = read_manifest.expect("manifest survives the round trip");
        assert_eq!(m.hostname, "testhost");
        assert_eq!(m.bootloaders, vec!["systemd-boot"]);

        assert_eq!(files.len(), 2, "manifest is not counted as a restored file");
        let contents: Vec<String> =
            files.iter().map(|(_, c)| String::from_utf8_lossy(c).into_owned()).collect();
        assert!(contents.iter().any(|c| c.contains("timeout 4")));
        assert!(contents.iter().any(|c| c.contains("title Arch")));
    }

    #[test]
    fn archived_paths_are_absolute_without_the_leading_slash() {
        let tree = TempTree::new("backup-paths");
        let f = tree.file("etc/config", "x\n");
        let archive = tree.path("b.tar.gz");

        let manifest = Manifest {
            version: 1,
            created: String::new(),
            hostname: String::new(),
            kernel: String::new(),
            architecture: String::new(),
            bootloaders: Vec::new(),
            files: Vec::new(),
        };
        write_archive(&archive, &[f.clone()], &manifest).unwrap();

        let (_, files) = read_archive(&archive).unwrap();
        let name = &files[0].0;
        // tar refuses absolute paths; prefixing '/' on restore recovers the
        // original location exactly.
        assert!(!name.starts_with('/'));
        assert_eq!(PathBuf::from("/").join(name), f);
    }

    #[test]
    fn archive_is_valid_gzip_and_not_truncated() {
        let tree = TempTree::new("backup-flush");
        // Large enough that a missing final flush would lose data.
        let big = "x".repeat(200_000);
        let f = tree.file("big.conf", &big);
        let archive = tree.path("b.tar.gz");

        let manifest = Manifest {
            version: 1,
            created: String::new(),
            hostname: String::new(),
            kernel: String::new(),
            architecture: String::new(),
            bootloaders: Vec::new(),
            files: Vec::new(),
        };
        write_archive(&archive, &[f], &manifest).unwrap();

        let (_, files) = read_archive(&archive).unwrap();
        assert_eq!(files[0].1.len(), big.len());
    }

    #[test]
    fn dedupe_keeps_one_entry_per_real_file() {
        let tree = TempTree::new("backup-dedupe");
        let f = tree.file("a.conf", "x");
        let deduped = dedupe(vec![f.clone(), f.clone(), tree.path("b.conf")]);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn reading_a_non_archive_is_an_error() {
        let tree = TempTree::new("backup-bad");
        let bad = tree.file("not-an-archive.tar.gz", "this is not gzip data");
        assert!(read_archive(&bad).is_err());
    }
}
