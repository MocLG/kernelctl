//! GRUB's persistent environment block.
//!
//! `grubenv` is how GRUB stores state it can write from inside the boot menu:
//! `saved_entry` (the default, when `GRUB_DEFAULT=saved`) and `next_entry`
//! (the one-shot, what `grub-reboot` sets).
//!
//! It is not an ordinary config file. GRUB rewrites it from early boot code
//! with no filesystem allocator available, so the file must stay **exactly**
//! 1024 bytes and must never move on disk. The layout is a fixed header, then
//! `name=value` lines, then `#` padding out to the full length. Writing a
//! shorter or longer file makes GRUB reject the block, so the padding is
//! reconstructed on every write and an over-full block is a hard error rather
//! than a truncation.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};

/// The exact size GRUB expects. `GRUB_ENVBLK_DEFLENGTH` in GRUB's source.
pub const BLOCK_SIZE: usize = 1024;

/// Header GRUB uses to recognise the block.
pub const SIGNATURE: &str = "# GRUB Environment Block\n";

/// A parsed environment block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrubEnv {
    /// Sorted so a rewrite is byte-stable when nothing changed.
    pub vars: BTreeMap<String, String>,
}

impl GrubEnv {
    /// Parse a block's raw bytes.
    pub fn parse(text: &str) -> Result<GrubEnv> {
        let body = text.strip_prefix(SIGNATURE).ok_or_else(|| {
            Error::other("not a GRUB environment block: signature header missing")
        })?;

        let mut vars = BTreeMap::new();
        for line in body.lines() {
            // Padding starts at the first '#' run and continues to the end.
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                vars.insert(k.to_string(), v.to_string());
            }
        }
        Ok(GrubEnv { vars })
    }

    pub fn load(path: &Path) -> Result<GrubEnv> {
        let text = crate::sys::atomic::read_to_string(path)?;
        GrubEnv::parse(&text).map_err(|e| Error::parse(path, None, e.to_string()))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// Set a variable.
    ///
    /// GRUB parses the block line by line, so a value containing a newline
    /// would silently corrupt the file and is rejected here instead.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if value.contains('\n') || key.contains('\n') || key.contains('=') {
            return Err(Error::validation(
                "GRUB environment values cannot contain newlines or '=' in the name",
            ));
        }
        self.vars.insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub fn remove(&mut self, key: &str) {
        self.vars.remove(key);
    }

    /// Render back to the exact 1024-byte on-disk form.
    pub fn render(&self) -> Result<Vec<u8>> {
        let mut out = String::from(SIGNATURE);
        for (k, v) in &self.vars {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }

        if out.len() > BLOCK_SIZE {
            return Err(Error::validation(format!(
                "GRUB environment block would be {} bytes, over the {BLOCK_SIZE}-byte limit; \
                 remove some variables first",
                out.len()
            )));
        }
        // Pad with '#' to the exact length GRUB requires.
        out.push_str(&"#".repeat(BLOCK_SIZE - out.len()));
        Ok(out.into_bytes())
    }

    /// A freshly initialized, empty block.
    pub fn empty() -> GrubEnv {
        GrubEnv::default()
    }
}

/// Write a block back in place.
///
/// GRUB may rewrite this file from boot code that cannot follow a moved
/// inode, so this deliberately does **not** use the atomic rename helper -
/// a rename would allocate a new inode and can leave GRUB unable to update
/// the block from the menu. The file is a fixed 1024 bytes and every write
/// replaces it whole, so a torn write cannot change its length.
pub fn write_in_place(path: &Path, env: &GrubEnv) -> Result<()> {
    let data = env.render()?;
    debug_assert_eq!(data.len(), BLOCK_SIZE);

    // Keep a .bak alongside, as every other config write does.
    if path.exists() {
        let bak = crate::sys::atomic::backup_path_for(path);
        std::fs::copy(path, &bak).map_err(|e| Error::io(&bak, e))?;
    }

    std::fs::write(path, &data).map_err(|e| Error::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let mut s = String::from(SIGNATURE);
        s.push_str("saved_entry=gnulinux-advanced-abc\n");
        s.push_str("boot_success=1\n");
        s.push_str(&"#".repeat(BLOCK_SIZE - s.len()));
        s
    }

    #[test]
    fn parses_a_block() {
        let env = GrubEnv::parse(&sample()).unwrap();
        assert_eq!(env.get("saved_entry"), Some("gnulinux-advanced-abc"));
        assert_eq!(env.get("boot_success"), Some("1"));
        assert_eq!(env.get("absent"), None);
    }

    #[test]
    fn rejects_a_file_without_the_signature() {
        let err = GrubEnv::parse("saved_entry=x\n").unwrap_err();
        assert!(err.to_string().contains("signature"));
    }

    #[test]
    fn renders_to_exactly_the_block_size() {
        let env = GrubEnv::parse(&sample()).unwrap();
        let rendered = env.render().unwrap();
        assert_eq!(rendered.len(), BLOCK_SIZE, "GRUB rejects a block of any other size");
        assert!(rendered.starts_with(SIGNATURE.as_bytes()));
        assert_eq!(*rendered.last().unwrap(), b'#', "unused space is '#' padded");
    }

    #[test]
    fn round_trips_without_changing_content() {
        let env = GrubEnv::parse(&sample()).unwrap();
        let rendered = String::from_utf8(env.render().unwrap()).unwrap();
        let reparsed = GrubEnv::parse(&rendered).unwrap();
        assert_eq!(env, reparsed);
    }

    #[test]
    fn sets_and_removes_variables() {
        let mut env = GrubEnv::parse(&sample()).unwrap();
        env.set("next_entry", "1>2").unwrap();
        assert_eq!(env.get("next_entry"), Some("1>2"));

        env.remove("next_entry");
        assert_eq!(env.get("next_entry"), None);

        // Removal must still render a valid block.
        assert_eq!(env.render().unwrap().len(), BLOCK_SIZE);
    }

    #[test]
    fn rejects_values_that_would_corrupt_the_block() {
        let mut env = GrubEnv::empty();
        // A newline would be parsed as the start of a new variable.
        assert!(env.set("k", "line1\nline2").is_err());
        assert!(env.set("bad=name", "v").is_err());
    }

    #[test]
    fn rejects_an_overfull_block() {
        let mut env = GrubEnv::empty();
        env.set("big", &"x".repeat(BLOCK_SIZE)).unwrap();
        let err = env.render().unwrap_err();
        assert!(err.to_string().contains("over the"));
    }

    #[test]
    fn empty_block_still_renders_valid() {
        let rendered = GrubEnv::empty().render().unwrap();
        assert_eq!(rendered.len(), BLOCK_SIZE);
        assert!(GrubEnv::parse(&String::from_utf8(rendered).unwrap()).unwrap().vars.is_empty());
    }

    #[test]
    fn writes_a_block_preserving_size_and_backing_up() {
        let dir = std::env::temp_dir().join(format!("kernelctl-grubenv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grubenv");
        std::fs::write(&path, sample()).unwrap();

        let mut env = GrubEnv::load(&path).unwrap();
        env.set("next_entry", "2").unwrap();
        write_in_place(&path, &env).unwrap();

        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), BLOCK_SIZE);
        assert_eq!(GrubEnv::load(&path).unwrap().get("next_entry"), Some("2"));
        // The original is preserved.
        let bak = crate::sys::atomic::backup_path_for(&path);
        assert!(GrubEnv::load(&bak).unwrap().get("next_entry").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
