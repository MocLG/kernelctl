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
//! Reading and writing UEFI variables through efivarfs.
//!
//! Two adapters need this: systemd-boot stores its default and one-shot entry
//! in vendor variables, and EFI-stub booting is entirely described by the
//! firmware's `Boot####` / `BootOrder` / `BootNext` variables.
//!
//! efivarfs has three quirks that make it unlike a normal file:
//!
//! - Each file is a 4-byte little-endian attribute word followed by the value.
//! - The attributes and the value must reach the kernel in a *single* write;
//!   a buffered writer that splits them corrupts the variable.
//! - Existing variables carry the immutable inode flag, so it has to be
//!   cleared before writing and restored afterwards.
//!
//! Strings in these variables are UTF-16LE with a NUL terminator, which is
//! what the firmware and systemd both expect.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Mount point of efivarfs on every Linux system.
pub const EFIVARS_DIR: &str = "/sys/firmware/efi/efivars";

/// `EFI_GLOBAL_VARIABLE` - Boot####, BootOrder, BootNext, BootCurrent.
pub const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// systemd's vendor GUID - LoaderEntryDefault, LoaderEntryOneShot, ...
pub const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// Non-volatile, readable at boot services time and at runtime. This is the
/// combination every boot-related variable uses; writing a different set makes
/// the firmware ignore the variable.
pub const ATTRS_NV_BS_RT: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004;

/// True when efivarfs is mounted and populated.
pub fn available() -> bool {
    fs::read_dir(EFIVARS_DIR).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// Path of a variable in efivarfs.
pub fn var_path(name: &str, guid: &str) -> PathBuf {
    Path::new(EFIVARS_DIR).join(format!("{name}-{guid}"))
}

/// Read a variable's value, with the attribute word stripped.
///
/// Returns `Ok(None)` when the variable simply does not exist, which is the
/// normal case for BootNext and LoaderEntryOneShot.
pub fn read(name: &str, guid: &str) -> Result<Option<Vec<u8>>> {
    let path = var_path(name, guid);
    match fs::read(&path) {
        Ok(raw) if raw.len() >= 4 => Ok(Some(raw[4..].to_vec())),
        // A variable that exists but is shorter than its attribute word is
        // firmware corruption, not an empty value.
        Ok(_) => Err(Error::parse(&path, None, "EFI variable is shorter than its attribute word")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// Read a variable as a UTF-16LE string.
pub fn read_string(name: &str, guid: &str) -> Result<Option<String>> {
    Ok(read(name, guid)?.map(|data| decode_utf16(&data)))
}

/// Write a variable, creating it if needed.
pub fn write(name: &str, guid: &str, attrs: u32, data: &[u8]) -> Result<()> {
    let path = var_path(name, guid);

    // The immutable flag is set on existing variables to stop stray writes.
    // Clear it, write, then put it back so we leave the system as we found it.
    let had_immutable = if path.exists() { clear_immutable(&path)? } else { false };

    let result = write_inner(&path, attrs, data);

    if had_immutable {
        // Restoring the flag is best effort: the write already happened, and
        // failing here must not turn a successful change into an error.
        let _ = set_immutable(&path);
    }
    result
}

fn write_inner(path: &Path, attrs: u32, data: &[u8]) -> Result<()> {
    // One buffer, one write: efivarfs rejects a value that arrives split
    // across multiple write() calls.
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&attrs.to_le_bytes());
    buf.extend_from_slice(data);

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| Error::io(path, e))?;

    file.write_all(&buf).map_err(|e| Error::io(path, e))?;
    Ok(())
}

/// Write a variable holding a UTF-16LE, NUL-terminated string.
pub fn write_string(name: &str, guid: &str, value: &str) -> Result<()> {
    write(name, guid, ATTRS_NV_BS_RT, &encode_utf16(value))
}

/// Delete a variable. Missing is treated as success, since the caller's intent
/// (clearing a one-shot, say) is already satisfied.
pub fn remove(name: &str, guid: &str) -> Result<()> {
    let path = var_path(name, guid);
    if !path.exists() {
        return Ok(());
    }
    clear_immutable(&path)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// Clear the immutable inode flag, reporting whether it had been set.
fn clear_immutable(path: &Path) -> Result<bool> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| Error::io(path, e))?;

    let flags = match rustix::fs::ioctl_getflags(&file) {
        Ok(f) => f,
        // Not every kernel or filesystem supports the ioctl. If we cannot read
        // the flags there is nothing to clear, so carry on and let the write
        // itself report a real failure.
        Err(_) => return Ok(false),
    };
    if !flags.contains(rustix::fs::IFlags::IMMUTABLE) {
        return Ok(false);
    }
    rustix::fs::ioctl_setflags(&file, flags & !rustix::fs::IFlags::IMMUTABLE)
        .map_err(|e| Error::io(path, e.into()))?;
    Ok(true)
}

fn set_immutable(path: &Path) -> Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    if let Ok(flags) = rustix::fs::ioctl_getflags(&file) {
        let _ = rustix::fs::ioctl_setflags(
            &file,
            flags | rustix::fs::IFlags::IMMUTABLE,
        );
    }
    Ok(())
}

/// Encode a string as UTF-16LE with the NUL terminator the firmware expects.
pub fn encode_utf16(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Decode UTF-16LE, stopping at the first NUL.
pub fn decode_utf16(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Decode a `BootOrder` variable: a packed array of little-endian u16s.
pub fn decode_boot_order(data: &[u8]) -> Vec<u16> {
    data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

/// Encode a `BootOrder` variable.
pub fn encode_boot_order(order: &[u16]) -> Vec<u8> {
    order.iter().flat_map(|n| n.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_utf16_strings() {
        let encoded = encode_utf16("arch.conf");
        // Nine characters plus the NUL terminator, two bytes each.
        assert_eq!(encoded.len(), 20);
        assert_eq!(&encoded[encoded.len() - 2..], &[0, 0]);
        assert_eq!(decode_utf16(&encoded), "arch.conf");
    }

    #[test]
    fn decodes_utf16_without_terminator() {
        let data: Vec<u8> = "abc".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_utf16(&data), "abc");
    }

    #[test]
    fn decode_stops_at_first_nul() {
        let mut data = encode_utf16("first");
        data.extend_from_slice(&encode_utf16("second"));
        assert_eq!(decode_utf16(&data), "first");
    }

    #[test]
    fn handles_non_ascii_titles() {
        assert_eq!(decode_utf16(&encode_utf16("Fedora Linux 40 (Wörkstation)")), "Fedora Linux 40 (Wörkstation)");
    }

    #[test]
    fn ignores_trailing_odd_byte() {
        // Firmware occasionally reports a length one byte longer than the
        // data; the stray byte must not panic the decoder.
        let mut data = encode_utf16("ok");
        data.push(0x41);
        assert_eq!(decode_utf16(&data), "ok");
    }

    #[test]
    fn round_trips_boot_order() {
        let order = vec![0x0003, 0x0000, 0x2001];
        let encoded = encode_boot_order(&order);
        assert_eq!(encoded, vec![0x03, 0x00, 0x00, 0x00, 0x01, 0x20]);
        assert_eq!(decode_boot_order(&encoded), order);
    }

    #[test]
    fn builds_variable_paths() {
        assert_eq!(
            var_path("BootNext", GLOBAL_GUID),
            PathBuf::from("/sys/firmware/efi/efivars/BootNext-8be4df61-93ca-11d2-aa0d-00e098032b8c")
        );
    }

    #[test]
    fn reading_a_missing_variable_is_not_an_error() {
        // Absent on any system, and absent on non-EFI systems entirely.
        assert_eq!(read("KernelctlNoSuchVar", LOADER_GUID).unwrap(), None);
    }
}
