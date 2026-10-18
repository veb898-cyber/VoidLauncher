//! Bundled vanilla default player skins (UUID parity with the game).
//!
//! A profile without a skin (offline accounts always; premium profiles on
//! fetch failure) is rendered by the client with a DEFAULT skin picked
//! deterministically from the profile UUID. To keep the launcher preview
//! consistent with what the game itself shows, we replicate the vanilla
//! algorithm exactly:
//!
//!   `DEFAULT_SKINS[Math.floorMod(uuid.hashCode(), DEFAULT_SKINS.length)]`
//!
//! verified against the official client bytecode (DefaultPlayerSkin, 1.21.1):
//! the array holds 18 entries — first the nine slim/ textures (alex, ari,
//! efe, kai, makena, noor, steve, sunny, zuri), then the same nine wide/
//! ones. Java's `UUID.hashCode()` XORs the two 64-bit halves and then both
//! 32-bit halves of the result — `((hilo >> 32) as i32) ^ (hilo as i32)` —
//! and `Math.floorMod` is Euclidean remainder, reproduced by `rem_euclid`.
//!
//! The PNGs are the vanilla 1.21.1 defaults extracted from the official
//! client jar; they are used exclusively for local skin previews.

/// (model, name, png) in the EXACT vanilla array order (slim ×9, wide ×9).
#[rustfmt::skip]
pub const DEFAULT_SKINS: [(&str, &str, &[u8]); 18] = [
    ("slim", "alex", include_bytes!("../assets/default_skins/slim/alex.png")),
    ("slim", "ari", include_bytes!("../assets/default_skins/slim/ari.png")),
    ("slim", "efe", include_bytes!("../assets/default_skins/slim/efe.png")),
    ("slim", "kai", include_bytes!("../assets/default_skins/slim/kai.png")),
    ("slim", "makena", include_bytes!("../assets/default_skins/slim/makena.png")),
    ("slim", "noor", include_bytes!("../assets/default_skins/slim/noor.png")),
    ("slim", "steve", include_bytes!("../assets/default_skins/slim/steve.png")),
    ("slim", "sunny", include_bytes!("../assets/default_skins/slim/sunny.png")),
    ("slim", "zuri", include_bytes!("../assets/default_skins/slim/zuri.png")),
    ("wide", "alex", include_bytes!("../assets/default_skins/wide/alex.png")),
    ("wide", "ari", include_bytes!("../assets/default_skins/wide/ari.png")),
    ("wide", "efe", include_bytes!("../assets/default_skins/wide/efe.png")),
    ("wide", "kai", include_bytes!("../assets/default_skins/wide/kai.png")),
    ("wide", "makena", include_bytes!("../assets/default_skins/wide/makena.png")),
    ("wide", "noor", include_bytes!("../assets/default_skins/wide/noor.png")),
    ("wide", "steve", include_bytes!("../assets/default_skins/wide/steve.png")),
    ("wide", "sunny", include_bytes!("../assets/default_skins/wide/sunny.png")),
    ("wide", "zuri", include_bytes!("../assets/default_skins/wide/zuri.png")),
];

/// Java `UUID.hashCode()` from the normalized 32-hex-char uuid (dashes
/// stripped, lowercase — same form as `heads::normalize_uuid`).
pub fn java_uuid_hash(norm_hex: &str) -> Option<i32> {
    if norm_hex.len() != 32 || !norm_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = hex::decode(norm_hex).ok()?;
    let msb = u64::from_be_bytes(bytes.get(0..8)?.try_into().ok()?);
    let lsb = u64::from_be_bytes(bytes.get(8..16)?.try_into().ok()?);
    // Java: long hilo = mostSigBits ^ leastSigBits;
    //       return ((int)(hilo >> 32)) ^ (int) hilo;
    let hilo = msb ^ lsb;
    Some(((hilo >> 32) as i32) ^ (hilo as i32))
}

/// Index into [`DEFAULT_SKINS`] for the given profile uuid.
pub fn default_skin_index(norm_hex: &str) -> Option<usize> {
    let hash = java_uuid_hash(norm_hex)?;
    Some(hash.rem_euclid(DEFAULT_SKINS.len() as i32) as usize)
}

/// The vanilla default skin PNG the game would show for this uuid.
pub fn default_skin_png(norm_hex: &str) -> Option<(&'static str, &'static str, &'static [u8])> {
    let idx = default_skin_index(norm_hex)?;
    DEFAULT_SKINS.get(idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values produced by JDK 21: uuid.hashCode() + Math.floorMod(h, 18).
    #[test]
    fn java_hash_and_index_match_the_jdk() {
        let cases = [
            ("d1379322167851f68a2a4d0cc3acb0ad", -1899413643, 15),
            ("069a79f444e94726a5befca90e38aaf5", -369792882, 0),
            ("00000000000000000000000000000000", 0, 0),
            ("ffffffffffffffffffffffffffffffff", 0, 0),
            ("7fffffffffffffffffffffffffffffff", -2147483648, 16),
            ("1a650540fb594d9da1d3f8f7a67379a3", -425932407, 9),
        ];
        for (uuid, hash, idx) in cases {
            assert_eq!(java_uuid_hash(uuid), Some(hash), "hash for {uuid}");
            assert_eq!(default_skin_index(uuid), Some(idx), "index for {uuid}");
        }
    }

    #[test]
    fn index_15_is_wide_steve_and_index_0_is_slim_alex() {
        // Matches the vanilla array order extracted from the 1.21.1 client.
        let (model, name, _) = default_skin_png("d1379322167851f68a2a4d0cc3acb0ad").unwrap();
        assert_eq!((model, name), ("wide", "steve"));
        let (model, name, _) = default_skin_png("069a79f444e94726a5befca90e38aaf5").unwrap();
        assert_eq!((model, name), ("slim", "alex"));
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert_eq!(java_uuid_hash("not-a-uuid"), None);
        assert_eq!(java_uuid_hash("c19dadb2de744d14888a635ea075492"), None); // 31 hex
        assert_eq!(default_skin_png("zz650540fb594d9da1d3f8f7a67379a3"), None);
    }

    #[test]
    fn every_bundled_skin_is_a_png() {
        const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        for (model, name, png) in DEFAULT_SKINS {
            assert!(png.starts_with(PNG_MAGIC), "{model}/{name} is not a PNG");
            assert!(png.len() > 24, "{model}/{name} is suspiciously small");
        }
    }
}
