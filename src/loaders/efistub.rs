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
//! Direct EFI boot: firmware NVRAM entries, no bootloader in between.
//!
//! When a kernel is built with CONFIG_EFI_STUB it is itself a PE executable
//! the firmware can launch, so the "boot menu" is the firmware's own
//! `Boot####` variable list ordered by `BootOrder`. There is no config file
//! anywhere; the entries live only in NVRAM.
//!
//! Rather than shelling out to `efibootmgr`, the `EFI_LOAD_OPTION` structures
//! are decoded directly, which means listing works on any system with
//! efivarfs mounted and no extra package installed. The structure is:
//!
//! ```text
//! u32  Attributes
//! u16  FilePathListLength
//! CHAR16 Description[]        NUL-terminated
//! u8   FilePathList[FilePathListLength]   packed device path nodes
//! u8   OptionalData[]         rest of the variable - the kernel cmdline
//! ```

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::model::{Arch, BootEntry, EntryFlags, LoaderKind};
use crate::sys::atomic::WriteOutcome;

use super::{efivars, scan::BootRoots, Bootloader, Capabilities, Context, Timeout};

/// LOAD_OPTION_ACTIVE - the firmware will only boot an entry with this set.
const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;

/// A decoded `Boot####` variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOption {
    pub number: u16,
    pub description: String,
    pub attributes: u32,
    /// Path from the device path's file-path node, in EFI backslash form.
    pub file_path: Option<String>,
    /// Trailing optional data, decoded as UTF-16 - for an EFI-stub kernel this
    /// is the command line.
    pub optional_data: Option<String>,
}

impl LoadOption {
    pub fn is_active(&self) -> bool {
        self.attributes & LOAD_OPTION_ACTIVE != 0
    }
}

/// Decode an `EFI_LOAD_OPTION`.
pub fn parse_load_option(number: u16, data: &[u8]) -> Option<LoadOption> {
    // Attributes plus the file path length is the minimum header.
    if data.len() < 6 {
        return None;
    }
    let attributes = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let path_len = u16::from_le_bytes([data[4], data[5]]) as usize;

    // The description is a NUL-terminated UTF-16 string starting at byte 6.
    let rest = &data[6..];
    let mut end = None;
    for (i, chunk) in rest.chunks_exact(2).enumerate() {
        if chunk == [0, 0] {
            end = Some(i * 2);
            break;
        }
    }
    let desc_bytes = end?;
    let description = efivars::decode_utf16(&rest[..desc_bytes]);

    // Skip the description and its terminator to reach the device path.
    let after_desc = desc_bytes + 2;
    let path_start = after_desc;
    let path_end = path_start.checked_add(path_len)?;
    if path_end > rest.len() {
        return None;
    }
    let file_path = extract_file_path(&rest[path_start..path_end]);

    // Anything after the device path is the optional data: for an EFI-stub
    // kernel, the command line.
    let optional_data = if path_end < rest.len() {
        let text = efivars::decode_utf16(&rest[path_end..]);
        (!text.trim().is_empty()).then_some(text)
    } else {
        None
    };

    Some(LoadOption { number, description, attributes, file_path, optional_data })
}

/// Walk a packed device path and return the media file-path node's contents.
///
/// Each node is `u8 type, u8 subtype, u16 length` followed by `length - 4`
/// bytes of payload. The node we want is type 0x04 (Media), subtype 0x04
/// (File Path), whose payload is a UTF-16 path such as `\EFI\Linux\arch.efi`.
fn extract_file_path(mut nodes: &[u8]) -> Option<String> {
    const TYPE_MEDIA: u8 = 0x04;
    const SUBTYPE_FILE_PATH: u8 = 0x04;
    const TYPE_END: u8 = 0x7F;

    while nodes.len() >= 4 {
        let node_type = nodes[0];
        let subtype = nodes[1];
        let length = u16::from_le_bytes([nodes[2], nodes[3]]) as usize;

        // A length below the header size would not advance and would loop
        // forever on malformed firmware data.
        if length < 4 || length > nodes.len() {
            return None;
        }
        if node_type == TYPE_END {
            return None;
        }
        if node_type == TYPE_MEDIA && subtype == SUBTYPE_FILE_PATH {
            return Some(efivars::decode_utf16(&nodes[4..length]));
        }
        nodes = &nodes[length..];
    }
    None
}

/// Enumerate every `Boot####` variable present in efivarfs.
fn read_boot_numbers() -> Vec<u16> {
    let Ok(dir) = std::fs::read_dir(efivars::EFIVARS_DIR) else { return Vec::new() };
    let mut numbers: Vec<u16> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Files are named `Boot0001-<guid>`; only the global GUID counts.
            let (var, guid) = name.split_once('-')?;
            if !guid.eq_ignore_ascii_case(efivars::GLOBAL_GUID) {
                return None;
            }
            let digits = var.strip_prefix("Boot")?;
            // `BootOrder`, `BootNext` and `BootCurrent` share the prefix but
            // are not entries.
            (digits.len() == 4).then(|| u16::from_str_radix(digits, 16).ok())?
        })
        .collect();
    numbers.sort_unstable();
    numbers
}

pub struct EfiStub {
    /// Boot roots, used to map an EFI path back onto a mounted file.
    esp_roots: Vec<PathBuf>,
}

impl EfiStub {
    pub fn detect(roots: &BootRoots) -> Option<EfiStub> {
        // Firmware entries belong to the running machine and have no
        // configurable location, so a scan aimed elsewhere must not report
        // them as if they came from the target being inspected.
        if !roots.host_state {
            return None;
        }
        // Without EFI variables there is nothing to read and nothing to write.
        if !efivars::available() {
            return None;
        }
        if read_boot_numbers().is_empty() {
            return None;
        }
        let mut esp_roots = roots.esp.clone();
        if esp_roots.is_empty() {
            esp_roots = roots.boot.clone();
        }
        Some(EfiStub { esp_roots })
    }

    /// Map an EFI path such as `\EFI\Linux\arch.efi` onto a mounted file.
    fn resolve(&self, efi_path: &str) -> Option<PathBuf> {
        let relative = efi_path.replace('\\', "/");
        let relative = relative.trim_start_matches('/');
        self.esp_roots.iter().map(|r| r.join(relative)).find(|p| p.exists())
    }

    fn boot_order(&self) -> Vec<u16> {
        efivars::read("BootOrder", efivars::GLOBAL_GUID)
            .ok()
            .flatten()
            .map(|d| efivars::decode_boot_order(&d))
            .unwrap_or_default()
    }

    fn boot_next(&self) -> Option<u16> {
        let data = efivars::read("BootNext", efivars::GLOBAL_GUID).ok()??;
        (data.len() >= 2).then(|| u16::from_le_bytes([data[0], data[1]]))
    }
}

impl Bootloader for EfiStub {
    fn kind(&self) -> LoaderKind {
        LoaderKind::EfiStub
    }

    fn capabilities(&self) -> Capabilities {
        // The firmware menu timeout is a separate variable that many firmwares
        // ignore, and entry contents cannot be edited without rewriting the
        // whole load option, so only ordering is offered.
        Capabilities::SET_DEFAULT | Capabilities::SET_ONESHOT
    }

    fn confidence(&self) -> u8 {
        // Almost every UEFI system has Boot#### entries, including ones that
        // then hand off to GRUB, so their presence alone is weak evidence that
        // this is how Linux is booted here.
        35
    }

    fn config_files(&self) -> Vec<PathBuf> {
        // NVRAM is not a file, so there is nothing for `backup` to archive.
        Vec::new()
    }

    fn post_write_note(&self) -> Option<String> {
        Some(
            "boot entries live in firmware NVRAM and are not covered by `kernelctl backup`; \
             record them with `efibootmgr -v` if you need a copy"
                .to_string(),
        )
    }

    fn entries(&self, _ctx: &Context) -> Result<Vec<BootEntry>> {
        let order = self.boot_order();
        let next = self.boot_next();
        let mut out = Vec::new();

        for number in read_boot_numbers() {
            let name = format!("Boot{number:04X}");
            let Ok(Some(data)) = efivars::read(&name, efivars::GLOBAL_GUID) else { continue };
            let Some(option) = parse_load_option(number, &data) else { continue };

            // An inactive entry is present but the firmware will not boot it.
            if !option.is_active() {
                continue;
            }

            let mut entry = BootEntry::new(
                LoaderKind::EfiStub,
                format!("efivars:{name}"),
                &name,
                if option.description.is_empty() { name.clone() } else { option.description.clone() },
            );

            if let Some(path) = &option.file_path {
                entry.extra.insert("efi-path".into(), path.clone());
                entry.kernel = self.resolve(path);
                // The suffix names the architecture of an EFI binary.
                if let Some(k) = &entry.kernel {
                    entry.arch = Arch::from_kernel_image(k).unwrap_or(Arch::Unknown);
                }
            }
            if let Some(cmdline) = &option.optional_data {
                entry.cmdline = cmdline.clone();
            }

            entry.flags.insert(EntryFlags::EFI_STUB);
            // The first entry in BootOrder is what the firmware boots.
            if order.first() == Some(&number) {
                entry.flags.insert(EntryFlags::DEFAULT);
            }
            if next == Some(number) {
                entry.flags.insert(EntryFlags::ONESHOT);
            }
            if let Some(pos) = order.iter().position(|n| *n == number) {
                entry.extra.insert("boot-order".into(), (pos + 1).to_string());
            }

            out.push(entry);
        }

        Ok(out)
    }

    fn set_default(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-default")?;
        let number = parse_boot_number(&entry.native_id)?;

        let mut order = self.boot_order();
        if order.is_empty() {
            return Err(Error::validation(
                "firmware reports no BootOrder, so the boot sequence cannot be changed",
            ));
        }
        // Move it to the front rather than replacing the list: the remaining
        // order is the user's fallback sequence and must be preserved.
        order.retain(|n| *n != number);
        order.insert(0, number);

        if ctx.dry_run {
            return Ok(Vec::new());
        }
        efivars::write(
            "BootOrder",
            efivars::GLOBAL_GUID,
            efivars::ATTRS_NV_BS_RT,
            &efivars::encode_boot_order(&order),
        )?;
        Ok(Vec::new())
    }

    fn set_oneshot(&self, ctx: &Context, entry: &BootEntry) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next")?;
        let number = parse_boot_number(&entry.native_id)?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        // The firmware deletes BootNext once it has been consumed.
        efivars::write(
            "BootNext",
            efivars::GLOBAL_GUID,
            efivars::ATTRS_NV_BS_RT,
            &number.to_le_bytes(),
        )?;
        Ok(Vec::new())
    }

    fn clear_oneshot(&self, ctx: &Context) -> Result<Vec<WriteOutcome>> {
        ctx.privileges.require("set-next --clear")?;
        if ctx.dry_run {
            return Ok(Vec::new());
        }
        efivars::remove("BootNext", efivars::GLOBAL_GUID)?;
        Ok(Vec::new())
    }

    fn timeout(&self, _ctx: &Context) -> Result<Option<Timeout>> {
        let Some(data) = efivars::read("Timeout", efivars::GLOBAL_GUID)? else { return Ok(None) };
        if data.len() < 2 {
            return Ok(None);
        }
        let secs = u16::from_le_bytes([data[0], data[1]]);
        Ok(Some(if secs == 0 { Timeout::Immediate } else { Timeout::Seconds(secs as u32) }))
    }
}

/// Recover the numeric part of a `Boot0003` identifier.
fn parse_boot_number(native_id: &str) -> Result<u16> {
    native_id
        .strip_prefix("Boot")
        .and_then(|d| u16::from_str_radix(d, 16).ok())
        .ok_or_else(|| Error::validation(format!("'{native_id}' is not an EFI boot entry id")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an EFI_LOAD_OPTION the way firmware lays one out.
    fn load_option(desc: &str, efi_path: &str, cmdline: Option<&str>, active: bool) -> Vec<u8> {
        // Media/File Path device path node, then an End node.
        let path_utf16 = efivars::encode_utf16(efi_path);
        let node_len = 4 + path_utf16.len();
        let mut device_path = Vec::new();
        device_path.push(0x04); // Media
        device_path.push(0x04); // File Path
        device_path.extend_from_slice(&(node_len as u16).to_le_bytes());
        device_path.extend_from_slice(&path_utf16);
        // End-of-device-path node.
        device_path.extend_from_slice(&[0x7F, 0xFF, 0x04, 0x00]);

        let mut out = Vec::new();
        out.extend_from_slice(&(if active { LOAD_OPTION_ACTIVE } else { 0 }).to_le_bytes());
        out.extend_from_slice(&(device_path.len() as u16).to_le_bytes());
        out.extend_from_slice(&efivars::encode_utf16(desc));
        out.extend_from_slice(&device_path);
        if let Some(c) = cmdline {
            out.extend_from_slice(&efivars::encode_utf16(c));
        }
        out
    }

    #[test]
    fn decodes_a_load_option() {
        let data = load_option("Arch Linux", "\\EFI\\Linux\\arch.efi", None, true);
        let opt = parse_load_option(1, &data).expect("decodes");

        assert_eq!(opt.description, "Arch Linux");
        assert_eq!(opt.file_path.as_deref(), Some("\\EFI\\Linux\\arch.efi"));
        assert!(opt.is_active());
        assert_eq!(opt.number, 1);
    }

    #[test]
    fn decodes_the_embedded_command_line() {
        let data = load_option("Linux", "\\vmlinuz.efi", Some("root=UUID=abc rw quiet"), true);
        let opt = parse_load_option(3, &data).unwrap();
        assert_eq!(opt.optional_data.as_deref(), Some("root=UUID=abc rw quiet"));
    }

    #[test]
    fn recognises_inactive_entries() {
        let data = load_option("Disabled", "\\x.efi", None, false);
        assert!(!parse_load_option(0, &data).unwrap().is_active());
    }

    #[test]
    fn handles_an_option_with_no_optional_data() {
        let data = load_option("Bare", "\\x.efi", None, true);
        assert_eq!(parse_load_option(0, &data).unwrap().optional_data, None);
    }

    #[test]
    fn rejects_truncated_data_without_panicking() {
        assert!(parse_load_option(0, &[]).is_none());
        assert!(parse_load_option(0, &[1, 2, 3]).is_none());
        // A header claiming a longer device path than exists.
        let mut data = load_option("X", "\\x.efi", None, true);
        data.truncate(10);
        assert!(parse_load_option(0, &data).is_none());
    }

    #[test]
    fn device_path_walk_terminates_on_malformed_nodes() {
        // A node length of zero would never advance the cursor.
        assert_eq!(extract_file_path(&[0x04, 0x04, 0x00, 0x00]), None);
        // A length past the end of the buffer is rejected rather than read.
        assert_eq!(extract_file_path(&[0x04, 0x04, 0xFF, 0xFF]), None);
        // An end node before any file path yields nothing.
        assert_eq!(extract_file_path(&[0x7F, 0xFF, 0x04, 0x00]), None);
    }

    #[test]
    fn skips_non_file_path_device_nodes() {
        // A hard-drive node (type 4, subtype 1) precedes the file path in
        // every real entry and must be stepped over, not misread.
        let path_utf16 = efivars::encode_utf16("\\EFI\\Linux\\a.efi");
        let mut nodes = vec![0x04, 0x01, 0x08, 0x00, 0, 0, 0, 0];
        nodes.push(0x04);
        nodes.push(0x04);
        nodes.extend_from_slice(&((4 + path_utf16.len()) as u16).to_le_bytes());
        nodes.extend_from_slice(&path_utf16);

        assert_eq!(extract_file_path(&nodes).as_deref(), Some("\\EFI\\Linux\\a.efi"));
    }

    #[test]
    fn parses_boot_numbers_from_identifiers() {
        assert_eq!(parse_boot_number("Boot0001").unwrap(), 1);
        assert_eq!(parse_boot_number("Boot000A").unwrap(), 10);
        assert_eq!(parse_boot_number("Boot2001").unwrap(), 0x2001);
        assert!(parse_boot_number("arch.conf").is_err());
    }
}
