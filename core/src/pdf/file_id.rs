//! The trailer's `/ID` (file identifier).
//!
//! The PDF spec asks for a value unique per document. Here it is a hash of every byte written
//! before the trailer, so documents that differ anywhere (text, fonts, metadata, dates) get
//! different identifiers, and identical input gives the identical identifier. The hash is
//! 128-bit FNV-1a: a fixed algorithm, so the value does not change with the Rust toolchain the
//! way `std`'s `DefaultHasher` may. It is not a cryptographic hash and does not need to be.
//! PDF/A requires a file identifier, so without one it does not conform.

const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// A running 128-bit FNV-1a hash, fed the file's bytes as they are produced.
pub(super) struct FileIdHasher(u128);

impl FileIdHasher {
    pub(super) fn new() -> Self {
        Self(OFFSET_BASIS)
    }

    pub(super) fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u128::from(byte)).wrapping_mul(PRIME);
        }
    }

    /// The 16 bytes written for both halves of `/ID` (this crate performs no incremental
    /// updates, so the permanent and the changing identifier are the same).
    pub(super) fn digest(&self) -> [u8; 16] {
        self.0.to_be_bytes()
    }
}

/// The `/ID` entry of the trailer dictionary, as hex strings.
pub(super) fn id_entry(id: &[u8; 16]) -> String {
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!("/ID [<{hex}> <{hex}>]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(bytes: &[u8]) -> [u8; 16] {
        let mut hasher = FileIdHasher::new();
        hasher.update(bytes);
        hasher.digest()
    }

    #[test]
    fn the_hash_is_fnv_1a_128_and_does_not_depend_on_chunking() {
        // Published FNV-1a 128-bit test vectors.
        assert_eq!(hash(b""), OFFSET_BASIS.to_be_bytes());
        assert_eq!(
            hash(b"a"),
            0xd228_cb69_6f1a_8caf_7891_2b70_4e4a_8964_u128.to_be_bytes()
        );

        let mut hasher = FileIdHasher::new();
        hasher.update(b"he");
        hasher.update(b"llo");
        assert_eq!(hasher.digest(), hash(b"hello"));
    }
}
