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
//! Unified Kernel Images.
//!
//! A UKI is a single PE/COFF executable bundling the EFI stub, the kernel, the
//! initrd, the command line and an os-release fragment as named sections. It
//! has no config file, so everything shown about one is read out of the binary
//! itself:
//!
//! - `.cmdline` - the baked-in kernel command line
//! - `.osrel`   - os-release fragment naming the OS and version
//! - `.linux` / `.initrd` - presence confirms it really is a UKI
//! - the COFF machine field gives the target architecture
//!
//! Reading these means the entry is genuinely informative rather than just a
//! filename. Nothing here is writable: changing a UKI's command line means
//! rebuilding and re-signing the image.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{Arch, BootEntry, EntryFlags, KernelVersion, LoaderKind};

use super::{scan::BootRoots, Bootloader, Capabilities, Context};

/// Sections a UKI is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UkiInfo {
    pub cmdline: Option<String>,
    /// Parsed `.osrel` keys.
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub arch: Arch,
    pub has_kernel: bool,
    pub has_initrd: bool,
}

/// Map a COFF machine value onto an architecture.
fn arch_from_machine(machine: u16) -> Arch {
    match machine {
        0x8664 => Arch::X86_64,
        0x014c => Arch::X86,
        0xAA64 => Arch::Aarch64,
        // 0x01c2 is ARM Thumb-2, which is what 32-bit ARM EFI binaries use.
        0x01c2 | 0x01c0 => Arch::Arm,
        0x5064 => Arch::Riscv64,
        0x6264 => Arch::Loongarch64,
        _ => Arch::Unknown,
    }
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
        *data.get(at + 2)?,
        *data.get(at + 3)?,
    ]))
}

/// Parse the PE section table and pull out the sections a UKI carries.
///
/// Deliberately tolerant: a malformed or truncated image yields whatever could
/// be read rather than an error, because a UKI we cannot fully parse is still
/// worth listing by filename.
pub fn parse_pe(data: &[u8]) -> Option<UkiInfo> {
    // The PE header offset lives at 0x3C in the DOS stub.
    if data.len() < 0x40 || &data[0..2] != b"MZ" {
        return None;
    }
    let pe_offset = read_u32(data, 0x3C)? as usize;
    if data.get(pe_offset..pe_offset + 4)? != b"PE\0\0" {
        return None;
    }

    // COFF header follows the signature.
    let coff = pe_offset + 4;
    let machine = read_u16(data, coff)?;
    let section_count = read_u16(data, coff + 2)? as usize;
    let optional_header_size = read_u16(data, coff + 16)? as usize;

    // The section table starts after the 20-byte COFF header and the optional
    // header, whose size the COFF header declares.
    let mut offset = coff + 20 + optional_header_size;

    let mut info = UkiInfo { arch: arch_from_machine(machine), ..Default::default() };

    // A corrupt section count could otherwise drive a very long loop.
    for _ in 0..section_count.min(96) {
        let Some(entry) = data.get(offset..offset + 40) else { break };

        // Section names are 8 bytes, NUL-padded.
        let name_end = entry[..8].iter().position(|b| *b == 0).unwrap_or(8);
        let name = String::from_utf8_lossy(&entry[..name_end]).into_owned();

        let raw_size = read_u32(entry, 16)? as usize;
        let raw_offset = read_u32(entry, 20)? as usize;
        let section = data.get(raw_offset..raw_offset.saturating_add(raw_size));

        match name.as_str() {
            ".cmdline" => {
                if let Some(bytes) = section {
                    let text = String::from_utf8_lossy(bytes);
                    let trimmed = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
                    if !trimmed.is_empty() {
                        info.cmdline = Some(trimmed.to_string());
                    }
                }
            }
            ".osrel" => {
                if let Some(bytes) = section {
                    let text = String::from_utf8_lossy(bytes);
                    info.os_name = osrel_get(&text, "PRETTY_NAME").or_else(|| osrel_get(&text, "NAME"));
                    info.os_version = osrel_get(&text, "VERSION_ID").or_else(|| osrel_get(&text, "VERSION"));
                }
            }
            ".linux" => info.has_kernel = true,
            ".initrd" => info.has_initrd = true,
            _ => {}
        }

        offset += 40;
    }

    Some(info)
}

/// Read one key out of an os-release fragment.
fn osrel_get(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let Some((k, v)) = line.trim().split_once('=') else { continue };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let unquoted = if v.len() >= 2 {
            let b = v.as_bytes();
            if (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
                &v[1..v.len() - 1]
            } else {
                v
            }
        } else {
            v
        };
        if !unquoted.is_empty() {
            return Some(unquoted.to_string());
        }
    }
    None
}

/// Read a UKI's metadata, reading only as much of the file as the header needs.
pub fn inspect(path: &Path) -> Option<UkiInfo> {
    // UKIs are tens of megabytes and we only need the headers plus two small
    // sections, but section payloads can sit anywhere in the file, so the
    // whole image is read and then discarded. Capped so a stray huge file
    // cannot exhaust memory.
    const MAX_READ: u64 = 256 * 1024 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_READ {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    parse_pe(&data)
}

pub struct Uki {
    /// Directories holding UKIs, in discovery order.
    dirs: Vec<PathBuf>,
}

impl Uki {
    /// Directories UKIs are conventionally installed into.
    const RELATIVE_DIRS: [&'static str; 2] = ["EFI/Linux", "EFI/BOOT/Linux"];

    pub fn detect(roots: &BootRoots) -> Option<Uki> {
        let mut dirs = Vec::new();
        for root in roots.esp.iter().chain(roots.boot.iter()) {
            for rel in Self::RELATIVE_DIRS {
                let dir = root.join(rel);
                if dir.is_dir() && !dirs.contains(&dir) && has_efi_image(&dir) {
                    dirs.push(dir);
                }
            }
        }
        (!dirs.is_empty()).then_some(Uki { dirs })
    }

    fn images(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flat_map(|d| d.flatten().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("efi")))
            .collect();
        out.sort();
        out
    }
}

fn has_efi_image(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut d| {
            d.any(|e| {
                e.map(|e| {
                    e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("efi"))
                })
                .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

impl Bootloader for Uki {
    fn kind(&self) -> LoaderKind {
        LoaderKind::Uki
    }

    fn capabilities(&self) -> Capabilities {
        // A UKI's command line and initrd are inside the signed binary, so
        // nothing here can be changed without rebuilding the image.
        Capabilities::NONE
    }

    fn confidence(&self) -> u8 {
        // UKIs are launched by a real bootloader or by firmware; on their own
        // they are a set of images, not a boot manager, so this adapter is a
        // fallback for when nothing else claims them.
        30
    }

    fn config_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "a unified kernel image bundles its kernel, initrd and command line into one \
             signed binary; changing any of them means rebuilding the image"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        Ok(self
            .images()
            .into_iter()
            .map(|path| {
                let file_name =
                    path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();

                let info = inspect(&path);

                // Prefer the OS name recorded inside the image; fall back to
                // the filename, which is all an unparseable image gives us.
                let title = match info.as_ref().and_then(|i| i.os_name.clone()) {
                    Some(name) => match info.as_ref().and_then(|i| i.os_version.clone()) {
                        Some(v) if !name.contains(&v) => format!("{name} {v}"),
                        _ => name,
                    },
                    None => stem.clone(),
                };

                let mut entry = BootEntry::new(LoaderKind::Uki, &path, &file_name, title);
                entry.kernel = Some(path.clone());
                entry.flags.insert(EntryFlags::UNIFIED | EntryFlags::EFI_STUB);

                if let Some(info) = info {
                    if let Some(c) = info.cmdline {
                        entry.cmdline = c;
                    }
                    entry.arch = info.arch;
                    if let Some(v) = &info.os_version {
                        entry.extra.insert("os-version".into(), v.clone());
                    }
                    entry.extra.insert(
                        "sections".into(),
                        format!(
                            "{}{}",
                            if info.has_kernel { ".linux " } else { "" },
                            if info.has_initrd { ".initrd" } else { "" }
                        )
                        .trim()
                        .to_string(),
                    );
                }
                // The version is usually only in the filename.
                entry.version = KernelVersion::from_filename(&stem);
                entry
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::testsupport::{Fixture, TempTree};

    /// Assemble a minimal but structurally valid PE with the given sections.
    fn build_pe(machine: u16, sections: &[(&str, &[u8])]) -> Vec<u8> {
        const OPTIONAL_HEADER_SIZE: usize = 240;
        let pe_offset = 0x80usize;
        let header_len = pe_offset + 4 + 20 + OPTIONAL_HEADER_SIZE + sections.len() * 40;
        // Payloads start on a round boundary after the headers.
        let mut payload_at = (header_len + 0x200) & !0x1FF;

        let mut out = vec![0u8; header_len];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3C..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        out[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");

        let coff = pe_offset + 4;
        out[coff..coff + 2].copy_from_slice(&machine.to_le_bytes());
        out[coff + 2..coff + 4].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        out[coff + 16..coff + 18]
            .copy_from_slice(&(OPTIONAL_HEADER_SIZE as u16).to_le_bytes());

        let table = coff + 20 + OPTIONAL_HEADER_SIZE;
        let mut payloads: Vec<(usize, &[u8])> = Vec::new();

        for (i, (name, data)) in sections.iter().enumerate() {
            let at = table + i * 40;
            let bytes = name.as_bytes();
            out[at..at + bytes.len().min(8)].copy_from_slice(&bytes[..bytes.len().min(8)]);
            out[at + 16..at + 20].copy_from_slice(&(data.len() as u32).to_le_bytes());
            out[at + 20..at + 24].copy_from_slice(&(payload_at as u32).to_le_bytes());
            payloads.push((payload_at, data));
            payload_at += data.len();
        }

        out.resize(payload_at, 0);
        for (at, data) in payloads {
            out[at..at + data.len()].copy_from_slice(data);
        }
        out
    }

    fn sample_uki() -> Vec<u8> {
        build_pe(
            0xAA64,
            &[
                (".linux", b"fake kernel"),
                (".initrd", b"fake initrd"),
                (".cmdline", b"root=UUID=abc rw quiet\0"),
                (
                    ".osrel",
                    b"NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nVERSION_ID=\"20260801\"\n",
                ),
            ],
        )
    }

    #[test]
    fn reads_sections_out_of_a_uki() {
        let info = parse_pe(&sample_uki()).expect("parses");
        assert_eq!(info.cmdline.as_deref(), Some("root=UUID=abc rw quiet"));
        assert_eq!(info.os_name.as_deref(), Some("Arch Linux"));
        assert_eq!(info.os_version.as_deref(), Some("20260801"));
        assert!(info.has_kernel);
        assert!(info.has_initrd);
    }

    #[test]
    fn reads_the_target_architecture_from_the_coff_header() {
        assert_eq!(parse_pe(&sample_uki()).unwrap().arch, Arch::Aarch64);
        assert_eq!(parse_pe(&build_pe(0x8664, &[])).unwrap().arch, Arch::X86_64);
        assert_eq!(parse_pe(&build_pe(0x01c2, &[])).unwrap().arch, Arch::Arm);
        assert_eq!(parse_pe(&build_pe(0x1234, &[])).unwrap().arch, Arch::Unknown);
    }

    #[test]
    fn rejects_files_that_are_not_pe_images() {
        assert!(parse_pe(b"not a pe file at all, just some bytes here....").is_none());
        assert!(parse_pe(&[]).is_none());
        // An MZ header with a bogus PE offset must not panic.
        let mut bad = vec![0u8; 0x40];
        bad[0..2].copy_from_slice(b"MZ");
        bad[0x3C..0x40].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(parse_pe(&bad).is_none());
    }

    #[test]
    fn tolerates_a_truncated_image() {
        let mut data = sample_uki();
        data.truncate(data.len() / 2);
        // Whatever is readable is returned; the point is not to panic.
        let _ = parse_pe(&data);
    }

    #[test]
    fn reads_osrel_values() {
        let text = "NAME=Fedora\nVERSION_ID=40\nPRETTY_NAME=\"Fedora Linux 40\"\n";
        assert_eq!(osrel_get(text, "PRETTY_NAME").as_deref(), Some("Fedora Linux 40"));
        assert_eq!(osrel_get(text, "VERSION_ID").as_deref(), Some("40"));
        assert_eq!(osrel_get(text, "MISSING"), None);
    }

    #[test]
    fn detects_and_describes_installed_images() {
        let tree = TempTree::new("uki-entries");
        let path = tree.path("EFI/Linux/arch-linux-6.12.1.efi");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, sample_uki()).unwrap();

        let fx = Fixture::rooted(tree.roots());
        let loader = Uki::detect(&fx.roots).expect("UKI directory detected");
        let entries = loader.entries(&fx.context()).unwrap();

        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        // The title comes from inside the image, not from the filename.
        assert_eq!(e.title, "Arch Linux 20260801");
        assert_eq!(e.cmdline, "root=UUID=abc rw quiet");
        assert_eq!(e.arch, Arch::Aarch64);
        assert!(e.flags.contains(EntryFlags::UNIFIED));
        assert!(e.flags.contains(EntryFlags::EFI_STUB));
        assert_eq!(e.version.as_ref().unwrap().raw, "6.12.1");
    }

    #[test]
    fn falls_back_to_the_filename_for_an_unreadable_image() {
        let tree = TempTree::new("uki-unreadable");
        tree.file("EFI/Linux/mystery-6.9.0.efi", "not really a PE file");

        let fx = Fixture::rooted(tree.roots());
        let loader = Uki::detect(&fx.roots).unwrap();
        let entries = loader.entries(&fx.context()).unwrap();

        assert_eq!(entries[0].title, "mystery-6.9.0");
        assert_eq!(entries[0].version.as_ref().unwrap().raw, "6.9.0");
    }

    #[test]
    fn ignores_a_directory_with_no_images() {
        let tree = TempTree::new("uki-empty");
        tree.dir("EFI/Linux");
        assert!(Uki::detect(&tree.roots()).is_none());
    }

    #[test]
    fn nothing_about_a_uki_is_writable() {
        let tree = TempTree::new("uki-readonly");
        tree.file("EFI/Linux/x.efi", "stub");
        let loader = Uki::detect(&tree.roots()).unwrap();
        assert_eq!(loader.capabilities(), Capabilities::NONE);
    }
}
