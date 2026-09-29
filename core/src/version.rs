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
