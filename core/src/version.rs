//! Client build numbers, read from the `Wow.exe` version resource.

use std::fmt;

/// A client file version such as `3.3.5.12340`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Expansion (3 = Wrath of the Lich King).
    pub major: u16,
    /// Major patch.
    pub minor: u16,
    /// Minor patch.
    pub patch: u16,
    /// Build number (12340 for 3.3.5a).
    pub build: u16,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.patch, self.build
        )
    }
}

impl Version {
    /// Reads the file version from a Windows executable's `VS_FIXEDFILEINFO`.
    ///
    /// Instead of walking the PE resource tree, this looks for the structure's
    /// signature followed by its fixed struct version, which is unambiguous in
    /// practice and survives the header edits private-server clients ship with.
    pub fn from_executable(bytes: &[u8]) -> Option<Self> {
        const SIGNATURE: [u8; 4] = 0xFEEF_04BDu32.to_le_bytes();
        const STRUCT_VERSION: [u8; 4] = 0x0001_0000u32.to_le_bytes();

        let word = |offset: usize| -> Option<u32> {
            let bytes = bytes.get(offset..offset + 4)?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        };

        bytes
            .windows(8)
            .enumerate()
            .filter(|(_, window)| window[..4] == SIGNATURE && window[4..] == STRUCT_VERSION)
            .find_map(|(offset, _)| {
                let most = word(offset + 8)?;
                let least = word(offset + 12)?;

                Some(Self {
                    major: (most >> 16) as u16,
                    minor: (most & 0xFFFF) as u16,
                    patch: (least >> 16) as u16,
                    build: (least & 0xFFFF) as u16,
                })
            })
    }

    /// Whether this is a 3.3.5 client, the only one supported.
    pub fn is_wotlk(&self) -> bool {
        (self.major, self.minor, self.patch) == (3, 3, 5)
    }
}

/// The `ProductVersion` a Windows program or DLL's version resource says,
/// as written there (`0.1.1`), if it has one: the string table's value, in
/// UTF-16 after its key and the padding, which is found the way the fixed
/// info is. A DLL standing in for one of Windows' own may say Windows'
/// version there, to pass for it.
pub fn product_version(bytes: &[u8]) -> Option<String> {
    let key: Vec<u8> = "ProductVersion\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut at = bytes.windows(key.len()).position(|window| window == key)? + key.len();
    // The value starts at a multiple of four.
    while bytes.get(at..at + 2) == Some(&[0, 0]) {
        at += 2;
    }

    let units: Vec<u16> = bytes
        .get(at..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .take(64)
        .collect();
    let version = String::from_utf16(&units).ok()?;
    let version = version.trim();

    (!version.is_empty() && version.chars().all(|c| !c.is_control())).then(|| version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{WOTLK, fake_executable};

    #[test]
    fn reads_the_fixed_file_info_of_an_executable() {
        let version = Version::from_executable(&fake_executable(WOTLK));

        assert_eq!(version, Some(WOTLK));
        assert_eq!(WOTLK.to_string(), "3.3.5.12340");
    }

    #[test]
    fn a_stray_signature_without_struct_version_is_ignored() {
        let mut bytes = 0xFEEF_04BDu32.to_le_bytes().to_vec();
        bytes.extend([0xAA; 16]);
        bytes.extend(fake_executable(WOTLK));

        assert_eq!(Version::from_executable(&bytes), Some(WOTLK));
    }

    #[test]
    fn files_without_version_info_have_no_version() {
        assert_eq!(Version::from_executable(b"MZ just some bytes"), None);
        assert_eq!(product_version(b"MZ just some bytes"), None);
    }

    #[test]
    fn the_product_version_is_read_from_the_string_table() {
        let utf16 =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        let mut bytes = b"MZ\0\0".to_vec();
        bytes.extend(utf16("FileVersion\0"));
        bytes.extend(utf16("10.0.17763.1\0"));
        bytes.extend([0, 0]);
        bytes.extend(utf16("ProductVersion\0"));
        bytes.extend([0, 0]);
        bytes.extend(utf16("0.1.1\0"));
        bytes.extend(utf16("ProductName\0Lumenveil\0"));

        assert_eq!(product_version(&bytes).as_deref(), Some("0.1.1"));

        let mut empty = b"MZ".to_vec();
        empty.extend(utf16("ProductVersion\0\0"));
        assert_eq!(product_version(&empty), None);
    }

    #[test]
    fn only_3_3_5_is_wotlk() {
        let cataclysm = Version {
            major: 4,
            minor: 3,
            patch: 4,
            build: 15595,
        };

        assert!(WOTLK.is_wotlk());
        assert!(!cataclysm.is_wotlk());
    }
}
