//! A small non-cryptographic hash used to mint stable entry identifiers.
//!
//! Entry ids only need to be stable across runs and collision-free within one
//! machine's boot config - a few dozen entries at most. FNV-1a is plenty for
//! that and costs nothing, where pulling in a hashing crate would add a
//! dependency for a hundred bytes of code.

/// FNV-1a, 64-bit.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Render a hash as a lowercase base36 string of exactly `len` characters,
/// left-padded with zeros so ids line up in a table.
fn base36(mut value: u64, len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = vec![b'0'; len];
    let mut i = len;
    while i > 0 && value > 0 {
        i -= 1;
        buf[i] = ALPHABET[(value % 36) as usize];
        value /= 36;
    }
    // Safe: every byte came from ASCII_ALPHABET.
    String::from_utf8(buf).expect("base36 alphabet is ASCII")
}

/// Six base36 characters - about 2.1 billion values, which makes an accidental
/// collision within a single boot menu effectively impossible.
pub fn short_hash(bytes: &[u8]) -> String {
    base36(fnv1a(bytes), 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_fnv1a_reference_vectors() {
        // Reference values from the FNV specification.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn short_hash_is_deterministic_and_fixed_width() {
        let a = short_hash(b"grub2\x01/boot/grub/grub.cfg\x01entry-1");
        let b = short_hash(b"grub2\x01/boot/grub/grub.cfg\x01entry-1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 6);
        assert_ne!(a, short_hash(b"grub2\x01/boot/grub/grub.cfg\x01entry-2"));
    }

    #[test]
    fn short_hash_is_lowercase_alphanumeric() {
        let h = short_hash(b"anything at all");
        assert!(h.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }
}
