//! Protection of the invitation secret at rest.
//!
//! `rooms.json` lives in the user's roaming profile. Windows DPAPI
//! (`CryptProtectData` with the current user) adds a meaningful layer against
//! copying the file to another account or machine, while keeping the existing
//! invitation protocol unchanged: the secret is decrypted only in memory.
//!
//! On non-Windows builds the value stays plaintext because DPAPI is
//! unavailable; the file format accepts both forms for migration.

/// Encode a secret for `rooms.json`. Windows returns `dpapi:<hex>`;
/// other platforms keep the legacy plaintext form.
pub fn encode(secret: &str) -> Option<String> {
    #[cfg(windows)]
    {
        protect(secret).map(|ciphertext| format!("dpapi:{}", hex::encode(ciphertext)))
    }
    #[cfg(not(windows))]
    {
        Some(secret.to_string())
    }
}

/// Decode a stored secret. Invalid/tampered DPAPI payloads yield `None`; the
/// room manager treats that as a missing secret and regenerates/resets it
/// according to the host/guest migration rules.
pub fn decode(stored: &str) -> Option<String> {
    if let Some(encoded) = stored.strip_prefix("dpapi:") {
        #[cfg(windows)]
        {
            let ciphertext = hex::decode(encoded).ok()?;
            let plaintext = unprotect(&ciphertext)?;
            if crate::rooms::room_state::valid_room_secret(&plaintext) {
                Some(plaintext)
            } else {
                None
            }
        }
        #[cfg(not(windows))]
        {
            let _ = encoded;
            tracing::warn!(
                target: "rooms",
                "A DPAPI-protected room secret was found on a non-Windows build; it will be regenerated/reset."
            );
            None
        }
    } else {
        // Legacy plaintext file (or a non-Windows build). The room manager
        // validates it before using it.
        Some(stored.to_string())
    }
}

#[cfg(windows)]
fn protect(plaintext: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = plaintext.as_bytes().to_vec();
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the input/output blobs are valid for the duration of the call;
    // Windows copies the result to memory owned by the caller (LocalFree
    // below). No prompt is allowed.
    let ok = unsafe {
        CryptProtectData(
            &input_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return None;
    }
    // SAFETY: Win32 returned a buffer of `cbData` bytes at `pbData`.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Some(bytes)
}

#[cfg(windows)]
fn unprotect(ciphertext: &[u8]) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = ciphertext.to_vec();
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: valid input/output blobs; no UI prompt is requested.
    let ok = unsafe {
        CryptUnprotectData(
            &input_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return None;
    }
    // SAFETY: Win32 returned a buffer of `cbData` bytes at `pbData`.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> String {
        crate::rooms::room_state::generate_room_secret()
    }

    #[test]
    fn legacy_plaintext_roundtrips_on_every_platform() {
        let value = secret();
        assert_eq!(decode(&value), Some(value.clone()));
        let encoded = encode(&value).expect("encode");
        assert_eq!(decode(&encoded), Some(value));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_hides_the_secret_and_rejects_tampering() {
        let value = secret();
        let encoded = encode(&value).expect("DPAPI encode");
        assert!(encoded.starts_with("dpapi:"));
        assert!(
            !encoded.contains(&value),
            "ciphertext must not contain plaintext"
        );
        assert_eq!(decode(&encoded), Some(value));

        let mut tampered = encoded.clone();
        let last = if tampered.ends_with('0') { '1' } else { '0' };
        tampered.pop();
        tampered.push(last);
        assert_eq!(
            decode(&tampered),
            None,
            "tampered ciphertext must be rejected"
        );
    }
}
