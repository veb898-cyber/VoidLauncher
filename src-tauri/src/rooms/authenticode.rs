//! Authenticode verification for the Tailscale MSI.
//!
//! The installer is fetched from a single HTTPS origin and its SHA-256 is
//! checked separately, but a compromised origin (or TLS endpoint) could serve
//! both a malicious MSI and its checksum. Before handing the MSI to elevated
//! `msiexec`, Windows must confirm that the file is signed by a trusted
//! certificate chain and by the expected Tailscale publisher.
//!
//! `WinVerifyTrust` performs the signature/chain validation. The signer
//! certificate is then extracted from the MSI's embedded PKCS#7 signature and
//! its simple-name is compared with [`EXPECTED_PUBLISHER`]. No certificate
//! thumbprint is pinned, so normal certificate rotation does not require a
//! launcher update; the trusted chain and exact publisher identity do.

use crate::error::{LauncherError, Result};
use std::path::Path;

/// The CN Windows reports for Tailscale's signed Windows installer.
pub const EXPECTED_PUBLISHER: &str = "Tailscale Inc.";

/// Small abstraction so the install path can be tested without a signed MSI.
pub trait InstallerSignatureVerifier: Send + Sync {
    fn verify(&self, path: &Path) -> Result<()>;
}

/// Verify the installer signature using the platform implementation.
pub fn verify_installer_signature(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        verify_with(path, &WindowsAuthenticodeVerifier)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Err(LauncherError::Launch(
            "Installer signature verification is only supported on Windows".into(),
        ))
    }
}

pub fn verify_with(path: &Path, verifier: &dyn InstallerSignatureVerifier) -> Result<()> {
    verifier.verify(path)
}

#[cfg(target_os = "windows")]
struct WindowsAuthenticodeVerifier;

#[cfg(target_os = "windows")]
impl InstallerSignatureVerifier for WindowsAuthenticodeVerifier {
    fn verify(&self, path: &Path) -> Result<()> {
        verify_windows_authenticode(path)
    }
}

#[cfg(target_os = "windows")]
fn verify_windows_authenticode(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Security::Cryptography::{
        CertCloseStore, CertFindCertificateInStore, CertFreeCertificateContext, CertGetNameStringW,
        CryptMsgClose, CryptMsgGetParam, CryptQueryObject, CERT_CONTEXT, CERT_FIND_SUBJECT_CERT,
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED,
        CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED, CERT_QUERY_FORMAT_FLAG_BINARY,
        CERT_QUERY_OBJECT_FILE, CMSG_SIGNER_CERT_INFO_PARAM, HCERTSTORE, PKCS_7_ASN_ENCODING,
        X509_ASN_ENCODING,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: ptr::null_mut(),
        pSIPClientData: ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: ptr::null_mut(),
        pwszURLReference: ptr::null_mut(),
        dwProvFlags: 0,
        dwUIContext: 0,
        pSignatureSettings: ptr::null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // SAFETY: all pointers reference live stack/local values for the duration
    // of the call; Windows does not retain them after it returns.
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        )
    };
    // Always close the trust state, even after a failed verification.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        );
    }
    if status != 0 {
        return Err(LauncherError::Launch(format!(
            "Tailscale installer signature is not trusted (WinVerifyTrust 0x{:08X})",
            status as u32
        )));
    }

    // WinVerifyTrust validates the signed MSI and its chain. Extract the
    // signer certificate to check the publisher identity as well.
    let mut h_store: HCERTSTORE = ptr::null_mut();
    let mut h_msg: *mut core::ffi::c_void = ptr::null_mut();
    let mut encoding = 0;
    let mut content = 0;
    let mut format = 0;
    // SAFETY: wide path is NUL-terminated; output pointers are valid.
    let queried = unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            wide.as_ptr() as *const core::ffi::c_void,
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED | CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            &mut encoding,
            &mut content,
            &mut format,
            &mut h_store,
            &mut h_msg,
            ptr::null_mut(),
        )
    };
    if queried == 0 || h_store.is_null() || h_msg.is_null() {
        if !h_store.is_null() {
            unsafe { CertCloseStore(h_store, 0) };
        }
        if !h_msg.is_null() {
            unsafe { CryptMsgClose(h_msg) };
        }
        return Err(LauncherError::Launch(
            "Could not read the Tailscale installer signature".into(),
        ));
    }

    let mut cert_info_len = 0u32;
    let got_len = unsafe {
        CryptMsgGetParam(
            h_msg,
            CMSG_SIGNER_CERT_INFO_PARAM,
            0,
            ptr::null_mut(),
            &mut cert_info_len,
        )
    };
    if got_len == 0 || cert_info_len == 0 {
        unsafe {
            CryptMsgClose(h_msg);
            CertCloseStore(h_store, 0);
        }
        return Err(LauncherError::Launch(
            "Could not read the Tailscale signer certificate".into(),
        ));
    }
    // Allocate with u64 alignment; CERT_INFO starts with pointer-sized fields.
    let words = (cert_info_len as usize).div_ceil(8);
    let mut cert_info_buf = vec![0u64; words];
    let mut actual_len = cert_info_len;
    let got_info = unsafe {
        CryptMsgGetParam(
            h_msg,
            CMSG_SIGNER_CERT_INFO_PARAM,
            0,
            cert_info_buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut actual_len,
        )
    };
    if got_info == 0 {
        unsafe {
            CryptMsgClose(h_msg);
            CertCloseStore(h_store, 0);
        }
        return Err(LauncherError::Launch(
            "Could not decode the Tailscale signer certificate".into(),
        ));
    }
    let cert_info =
        cert_info_buf.as_ptr() as *const windows_sys::Win32::Security::Cryptography::CERT_INFO;
    // SAFETY: cert_info points at a valid CERT_INFO returned by crypt32.
    let cert: *mut CERT_CONTEXT = unsafe {
        CertFindCertificateInStore(
            h_store,
            X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
            0,
            CERT_FIND_SUBJECT_CERT,
            cert_info as *const core::ffi::c_void,
            ptr::null_mut(),
        )
    };
    if cert.is_null() {
        unsafe {
            CryptMsgClose(h_msg);
            CertCloseStore(h_store, 0);
        }
        return Err(LauncherError::Launch(
            "Tailscale signer certificate is missing".into(),
        ));
    }

    let name_len = unsafe {
        CertGetNameStringW(
            cert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    let mut name = vec![0u16; name_len.max(1) as usize];
    let copied = if name_len > 0 {
        unsafe {
            CertGetNameStringW(
                cert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                ptr::null(),
                name.as_mut_ptr(),
                name.len() as u32,
            )
        }
    } else {
        0
    };
    let publisher = if copied > 0 {
        String::from_utf16_lossy(&name[..copied.saturating_sub(1) as usize])
    } else {
        String::new()
    };

    unsafe {
        CertFreeCertificateContext(cert);
        CryptMsgClose(h_msg);
        CertCloseStore(h_store, 0);
    }

    if !publisher.eq_ignore_ascii_case(EXPECTED_PUBLISHER) {
        return Err(LauncherError::Launch(format!(
            "Tailscale installer publisher '{}' is not the expected '{}'",
            publisher, EXPECTED_PUBLISHER
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(bool);

    impl InstallerSignatureVerifier for Fake {
        fn verify(&self, _path: &Path) -> Result<()> {
            if self.0 {
                Ok(())
            } else {
                Err(LauncherError::Launch("unsigned".into()))
            }
        }
    }

    #[test]
    fn verifier_abstraction_propagates_success_and_failure() {
        let path = Path::new("installer.msi");
        assert!(verify_with(path, &Fake(true)).is_ok());
        assert!(verify_with(path, &Fake(false)).is_err());
    }

    #[test]
    fn unsigned_temp_file_is_rejected_on_windows() {
        #[cfg(target_os = "windows")]
        {
            let dir = std::env::temp_dir().join(format!(
                "vl_authenticode_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("unsigned.msi");
            std::fs::write(&file, b"not a signed installer").unwrap();
            assert!(verify_installer_signature(&file).is_err());
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// Manual smoke check for a real downloaded MSI. Run with
    /// `VL_SIGNED_MSI=<path> cargo test --lib rooms::authenticode::tests::signed_msi -- --ignored`.
    #[test]
    #[ignore]
    fn signed_msi_passes_when_provided() {
        let Ok(path) = std::env::var("VL_SIGNED_MSI") else {
            return;
        };
        verify_installer_signature(Path::new(&path)).expect("signed MSI must verify");
    }
}
