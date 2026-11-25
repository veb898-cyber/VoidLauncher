//! Content-integrity primitives shared by every download path.
//!
//! `HashSpec` represents a hash that a caller is required to verify. It is
//! deliberately strict: an empty or malformed value cannot be turned into a
//! "no verification" request by accident. The existing SHA-1-only
//! [`crate::download::download_file`] API remains as a compatibility wrapper;
//! new callers should use `download_file_verified`.
//!
//! A hash only proves that the bytes match the value the caller supplied. It
//! says nothing about whether that value is authentic: the caller must know
//! where the hash came from. In particular, the local integrity manifest in
//! `download.rs` records the bytes received by this launcher; it detects later
//! local corruption but does not authenticate the original publisher.

use crate::error::{LauncherError, Result};
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

const SHA1_HEX_LEN: usize = 40;
const SHA256_HEX_LEN: usize = 64;
const SHA512_HEX_LEN: usize = 128;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// A validated hash value that must be checked against downloaded bytes.
///
/// The inner representation is private so callers cannot construct an empty or
/// malformed value by naming a public enum variant. Use [`HashSpec::parse`],
/// [`HashSpec::sha1`], [`HashSpec::sha256`], [`HashSpec::sha512`], or
/// [`HashSpec::from_metadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashSpec {
    algorithm: HashAlgorithm,
    hex: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => SHA1_HEX_LEN,
            Self::Sha256 => SHA256_HEX_LEN,
            Self::Sha512 => SHA512_HEX_LEN,
        }
    }
}

impl HashSpec {
    /// Parse a hash from an algorithm name and a hexadecimal value.
    ///
    /// Accepted algorithm spellings are `sha1`, `sha-1`, `sha256`,
    /// `sha-256`, `sha512`, and `sha-512` (case-insensitive). The value must
    /// be exactly the hexadecimal length for that algorithm; empty strings and
    /// unknown algorithms are rejected.
    pub fn parse(algorithm: &str, value: &str) -> Result<Self> {
        let algorithm = match algorithm.trim().to_ascii_lowercase().as_str() {
            "sha1" | "sha-1" => HashAlgorithm::Sha1,
            "sha256" | "sha-256" => HashAlgorithm::Sha256,
            "sha512" | "sha-512" => HashAlgorithm::Sha512,
            other => {
                return Err(LauncherError::Download(format!(
                    "Unsupported hash algorithm '{}'",
                    other
                )))
            }
        };

        let value = value.trim();
        let expected_len = algorithm.hex_len();
        if value.len() != expected_len || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(LauncherError::Download(format!(
                "Invalid {} hash: expected {} hexadecimal characters",
                algorithm.name().to_ascii_uppercase(),
                expected_len
            )));
        }

        Ok(Self {
            algorithm,
            hex: value.to_ascii_lowercase(),
        })
    }

    /// Construct a validated SHA-1 hash.
    pub fn sha1(value: &str) -> Result<Self> {
        Self::parse("sha1", value)
    }

    /// Construct a validated SHA-256 hash.
    #[allow(dead_code)]
    pub fn sha256(value: &str) -> Result<Self> {
        Self::parse("sha256", value)
    }

    /// Construct a validated SHA-512 hash.
    #[allow(dead_code)]
    pub fn sha512(value: &str) -> Result<Self> {
        Self::parse("sha512", value)
    }

    /// Pick the strongest hash available in provider metadata.
    ///
    /// Preference order is SHA-512, SHA-256, SHA-1. A missing or empty
    /// algorithm entry is not considered provided; a non-empty malformed
    /// value that the source **did** provide is an error, not a reason to
    /// silently fall back to a weaker algorithm. If no algorithm is present,
    /// the result is `Ok(None)`.
    ///
    /// The content-install PR uses this to translate provider metadata into a
    /// `HashSpec`; it is tested here so the translation cannot regress.
    #[allow(dead_code)]
    pub fn from_metadata(hashes: &HashMap<String, String>) -> Result<Option<Self>> {
        for (algorithm, keys) in [
            ("sha512", ["sha512", "sha-512"]),
            ("sha256", ["sha256", "sha-256"]),
            ("sha1", ["sha1", "sha-1"]),
        ] {
            for key in keys {
                let Some(value) = hashes.get(key) else {
                    continue;
                };
                if value.is_empty() {
                    // An exactly empty field means the source did not provide
                    // that hash. Whitespace-only is not the same thing: it is
                    // a malformed value and is rejected below.
                    continue;
                }
                let spec = Self::parse(algorithm, value).map_err(|_| {
                    LauncherError::Download(format!(
                        "Invalid {} hash in provider metadata",
                        algorithm.to_ascii_uppercase()
                    ))
                })?;
                return Ok(Some(spec));
            }
        }
        Ok(None)
    }

    /// Return a validated SHA-1 spec, or `None` for an empty value.
    ///
    /// A non-empty malformed value is an error. This is important for the
    /// legacy `download_file` wrapper: it must never reinterpret a bad hash as
    /// "no hash".
    pub fn from_sha1(value: &str) -> Result<Option<Self>> {
        if value.is_empty() {
            Ok(None)
        } else {
            Self::sha1(value).map(Some)
        }
    }

    /// Lowercase algorithm name (`sha1`, `sha256`, `sha512`).
    pub fn algorithm(&self) -> &'static str {
        self.algorithm.name()
    }

    /// Lowercase hexadecimal digest.
    #[allow(dead_code)]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Payload used in `.part` identity files.
    ///
    /// SHA-1 keeps the historical bare-hex form so existing resumable
    /// downloads keep their identity. Stronger algorithms are prefixed to
    /// avoid mixing artifacts with the same hex prefix under another hash.
    pub fn identity_payload(&self) -> String {
        match self.algorithm {
            HashAlgorithm::Sha1 => self.hex.clone(),
            HashAlgorithm::Sha256 => format!("sha256:{}", self.hex),
            HashAlgorithm::Sha512 => format!("sha512:{}", self.hex),
        }
    }

    /// Verify a file without loading it into memory.
    ///
    /// Comparison is case-insensitive because hashes are hexadecimal values,
    /// not case-sensitive identifiers.
    pub fn verify_file(&self, path: &Path) -> Result<bool> {
        let actual = hash_file(path, self.algorithm)?;
        Ok(actual.eq_ignore_ascii_case(&self.hex))
    }

    /// Verify an in-memory payload. Used for small metadata documents whose
    /// checksum sidecar is fetched before parsing; never for large artifacts.
    pub fn verify_bytes(&self, bytes: &[u8]) -> bool {
        let actual = match self.algorithm {
            HashAlgorithm::Sha1 => hex::encode(Sha1::digest(bytes)),
            HashAlgorithm::Sha256 => hex::encode(Sha256::digest(bytes)),
            HashAlgorithm::Sha512 => hex::encode(Sha512::digest(bytes)),
        };
        actual.eq_ignore_ascii_case(&self.hex)
    }
}

/// Hash a file with a validated algorithm. Used by [`HashSpec`] and by the
/// download module's compatibility wrappers.
fn hash_file(path: &Path, algorithm: HashAlgorithm) -> Result<String> {
    match algorithm {
        HashAlgorithm::Sha1 => hash_file_with::<Sha1>(path),
        HashAlgorithm::Sha256 => hash_file_with::<Sha256>(path),
        HashAlgorithm::Sha512 => hash_file_with::<Sha512>(path),
    }
}

fn hash_file_with<D: Digest>(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = D::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute a SHA-1 hash as lowercase hexadecimal.
pub fn hash_file_sha1(path: &Path) -> Result<String> {
    hash_file_with::<Sha1>(path)
}

/// Compute a SHA-256 hash as lowercase hexadecimal.
pub fn hash_file_sha256(path: &Path) -> Result<String> {
    hash_file_with::<Sha256>(path)
}

/// Compute a SHA-512 hash as lowercase hexadecimal.
///
/// The download layer itself does not need this helper yet; it is exercised by
/// the integrity tests and will be used by the content-install migration.
#[allow(dead_code)]
pub fn hash_file_sha512(path: &Path) -> Result<String> {
    hash_file_with::<Sha512>(path)
}

/// Verify a SHA-1 hash. Kept case-sensitive for compatibility with the
/// historical `download::verify_sha1` behavior; new code should use
/// [`HashSpec::verify_file`].
pub fn verify_sha1(path: &Path, expected: &str) -> Result<bool> {
    Ok(hash_file_sha1(path)? == expected)
}

/// Verify a SHA-256 hash.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<bool> {
    Ok(hash_file_sha256(path)?.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_integrity_{}_{}_{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA512_ABC: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

    #[test]
    fn parse_accepts_the_three_supported_algorithms() {
        assert_eq!(
            HashSpec::parse("sha1", SHA1_ABC).unwrap(),
            HashSpec::sha1(SHA1_ABC).unwrap()
        );
        assert_eq!(
            HashSpec::parse("sha-256", SHA256_ABC).unwrap(),
            HashSpec::sha256(SHA256_ABC).unwrap()
        );
        assert_eq!(
            HashSpec::parse("SHA-512", SHA512_ABC).unwrap(),
            HashSpec::sha512(SHA512_ABC).unwrap()
        );
    }

    #[test]
    fn parse_normalizes_case_and_rejects_empty_or_malformed_values() {
        assert_eq!(
            HashSpec::parse("sha1", &SHA1_ABC.to_ascii_uppercase()).unwrap(),
            HashSpec::sha1(SHA1_ABC).unwrap()
        );
        for value in [
            "",
            "   ",
            "abc",
            &"a".repeat(39),
            &"a".repeat(41),
            &"z".repeat(40),
        ] {
            assert!(
                HashSpec::parse("sha1", value).is_err(),
                "value must be rejected: {value:?}"
            );
        }
        assert!(HashSpec::parse("md5", "d41d8cd98f00b204e9800998ecf8427e").is_err());
    }

    #[test]
    fn validated_constructors_reject_empty_and_malformed_values() {
        assert!(HashSpec::sha1("").is_err());
        assert!(HashSpec::sha1("  ").is_err());
        assert!(HashSpec::sha256("not-a-hash").is_err());
        assert!(HashSpec::sha512(&"a".repeat(64)).is_err());
    }

    #[test]
    fn from_sha1_distinguishes_empty_from_invalid() {
        assert!(HashSpec::from_sha1("").unwrap().is_none());
        assert!(HashSpec::from_sha1("  ").is_err());
        assert!(HashSpec::from_sha1("not-a-hash").is_err());
        assert_eq!(
            HashSpec::from_sha1(SHA1_ABC).unwrap(),
            Some(HashSpec::sha1(SHA1_ABC).unwrap())
        );
    }

    #[test]
    fn from_metadata_prefers_the_strongest_valid_hash() {
        let mut hashes = HashMap::new();
        hashes.insert("sha1".to_string(), SHA1_ABC.to_string());
        hashes.insert("sha256".to_string(), SHA256_ABC.to_string());
        hashes.insert("sha512".to_string(), SHA512_ABC.to_string());
        assert_eq!(
            HashSpec::from_metadata(&hashes).unwrap(),
            Some(HashSpec::sha512(SHA512_ABC).unwrap())
        );

        hashes.remove("sha512");
        assert_eq!(
            HashSpec::from_metadata(&hashes).unwrap(),
            Some(HashSpec::sha256(SHA256_ABC).unwrap())
        );

        hashes.remove("sha256");
        assert_eq!(
            HashSpec::from_metadata(&hashes).unwrap(),
            Some(HashSpec::sha1(SHA1_ABC).unwrap())
        );
    }

    #[test]
    fn from_metadata_rejects_a_provided_malformed_hash_instead_of_falling_back() {
        let mut hashes = HashMap::new();
        hashes.insert("sha512".to_string(), "not-a-hash".to_string());
        hashes.insert("sha256".to_string(), SHA256_ABC.to_string());
        assert!(
            HashSpec::from_metadata(&hashes).is_err(),
            "a malformed SHA-512 must not silently fall back to SHA-256"
        );

        let mut hashes = HashMap::new();
        hashes.insert("sha256".to_string(), "not-a-hash".to_string());
        hashes.insert("sha1".to_string(), SHA1_ABC.to_string());
        assert!(
            HashSpec::from_metadata(&hashes).is_err(),
            "a malformed SHA-256 must not silently fall back to SHA-1"
        );

        let mut hashes = HashMap::new();
        hashes.insert("sha1".to_string(), "not-a-hash".to_string());
        assert!(HashSpec::from_metadata(&hashes).is_err());
    }

    #[test]
    fn from_metadata_treats_empty_entries_as_absent() {
        let mut hashes = HashMap::new();
        hashes.insert("sha512".to_string(), String::new());
        hashes.insert("sha256".to_string(), SHA256_ABC.to_string());
        assert_eq!(
            HashSpec::from_metadata(&hashes).unwrap(),
            Some(HashSpec::sha256(SHA256_ABC).unwrap())
        );

        let mut empty = HashMap::new();
        empty.insert("sha1".to_string(), String::new());
        assert_eq!(HashSpec::from_metadata(&empty).unwrap(), None);

        assert_eq!(HashSpec::from_metadata(&HashMap::new()).unwrap(), None);
    }

    #[test]
    fn from_metadata_rejects_whitespace_only_values() {
        for (algorithm, value) in [("sha512", "  "), ("sha256", "\t"), ("sha1", "   ")] {
            let mut hashes = HashMap::new();
            hashes.insert(algorithm.to_string(), value.to_string());
            assert!(
                HashSpec::from_metadata(&hashes).is_err(),
                "whitespace-only {algorithm} must be invalid"
            );
        }
    }

    #[test]
    fn identity_payload_keeps_sha1_backwards_compatible_and_prefixes_stronger_hashes() {
        assert_eq!(
            HashSpec::sha1(SHA1_ABC).unwrap().identity_payload(),
            SHA1_ABC
        );
        assert_eq!(
            HashSpec::sha256(SHA256_ABC).unwrap().identity_payload(),
            format!("sha256:{SHA256_ABC}")
        );
        assert_eq!(
            HashSpec::sha512(SHA512_ABC).unwrap().identity_payload(),
            format!("sha512:{SHA512_ABC}")
        );
    }

    #[test]
    fn verify_file_matches_each_algorithm_and_rejects_mismatch() {
        let path = temp_file("verify.bin", b"abc");
        assert!(HashSpec::sha1(SHA1_ABC)
            .unwrap()
            .verify_file(&path)
            .unwrap());
        assert!(HashSpec::sha256(SHA256_ABC)
            .unwrap()
            .verify_file(&path)
            .unwrap());
        assert!(HashSpec::sha512(SHA512_ABC)
            .unwrap()
            .verify_file(&path)
            .unwrap());
        assert!(!HashSpec::sha256(&"0".repeat(64))
            .unwrap()
            .verify_file(&path)
            .unwrap());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn compatibility_hash_helpers_match_known_vectors() {
        let path = temp_file("vectors.bin", b"abc");
        assert_eq!(hash_file_sha1(&path).unwrap(), SHA1_ABC);
        assert_eq!(hash_file_sha256(&path).unwrap(), SHA256_ABC);
        assert_eq!(hash_file_sha512(&path).unwrap(), SHA512_ABC);
        assert!(verify_sha1(&path, SHA1_ABC).unwrap());
        assert!(verify_sha256(&path, SHA256_ABC).unwrap());
        assert!(HashSpec::sha512(SHA512_ABC)
            .unwrap()
            .verify_file(&path)
            .unwrap());
        assert!(!verify_sha1(&path, &"0".repeat(40)).unwrap());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
