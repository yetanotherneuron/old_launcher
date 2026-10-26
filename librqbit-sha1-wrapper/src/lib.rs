//! The hashes librqbit checks torrents with, SHA-1 and SHA-256 (BitTorrent
//! v2), behind the interface of the crate this one replaces.

/// A SHA-1 hasher.
pub trait ISha1 {
    /// A new hasher.
    fn new() -> Self;
    /// Hashes `buf` next.
    fn update(&mut self, buf: &[u8]);
    /// The hash of everything given.
    fn finish(self) -> [u8; 20];
}

/// A SHA-256 hasher.
pub trait ISha256 {
    /// A new hasher.
    fn new() -> Self;
    /// Hashes `buf` next.
    fn update(&mut self, buf: &[u8]);
    /// The hash of everything given.
    fn finish(self) -> [u8; 32];

    /// The same, as a BitTorrent v2 id.
    fn finish_id32(self) -> [u8; 32]
    where
        Self: Sized,
    {
        self.finish()
    }
}

/// SHA-1, by RustCrypto.
pub struct Sha1(sha1::Sha1);

impl ISha1 for Sha1 {
    fn new() -> Self {
        Self(<sha1::Sha1 as sha1::Digest>::new())
    }

    fn update(&mut self, buf: &[u8]) {
        sha1::Digest::update(&mut self.0, buf);
    }

    fn finish(self) -> [u8; 20] {
        sha1::Digest::finalize(self.0).into()
    }
}

/// SHA-256, by RustCrypto.
pub struct Sha256(sha2::Sha256);

impl ISha256 for Sha256 {
    fn new() -> Self {
        Self(<sha2::Sha256 as sha2::Digest>::new())
    }

    fn update(&mut self, buf: &[u8]) {
        sha2::Digest::update(&mut self.0, buf);
    }

    fn finish(self) -> [u8; 32] {
        sha2::Digest::finalize(self.0).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn the_hashes_are_the_standard_ones() {
        let mut sha1 = <Sha1 as ISha1>::new();
        sha1.update(b"ab");
        sha1.update(b"c");
        assert_eq!(
            hex(&sha1.finish()),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );

        let mut sha256 = <Sha256 as ISha256>::new();
        sha256.update(b"");
        assert_eq!(
            hex(&sha256.finish()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
