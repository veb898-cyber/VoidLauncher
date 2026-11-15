//! Saving generated text exports (currently the mod list).
//!
//! The frontend builds the document and asks the native save dialog for a
//! path; this command only performs the UTF-8 write. Keeping the write in
//! Rust gives us the encoder control the webview lacks (an optional BOM for
//! CSV/Excel) and a deterministic error string for the UI.

use std::path::{Path, PathBuf};

/// The generated list is a few hundred kilobytes even for a huge pack; the
/// cap only protects the command from being used as a bulk file writer.
const MAX_EXPORT_BYTES: usize = 32 * 1024 * 1024;

#[tauri::command]
pub async fn cmd_save_export_file(
    path: String,
    content: String,
    add_utf8_bom: bool,
) -> Result<(), String> {
    if content.len() > MAX_EXPORT_BYTES {
        return Err("The export is too large to save.".to_string());
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("The export path must be absolute.".to_string());
    }
    if path.file_name().is_none() {
        return Err("The export path does not point to a file.".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        write_export_file(&path, &content, add_utf8_bom).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// UTF-8 bytes with an optional BOM. Excel needs the BOM to detect UTF-8 in
/// `.csv`; every other format is written without it, as browsers and editors
/// expect plain UTF-8.
pub(crate) fn encode_export_text(content: &str, add_utf8_bom: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(content.len() + 3);
    if add_utf8_bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(content.as_bytes());
    bytes
}

pub(crate) fn write_export_file(
    path: &Path,
    content: &str,
    add_utf8_bom: bool,
) -> std::io::Result<()> {
    std::fs::write(path, encode_export_text(content, add_utf8_bom))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_is_only_added_when_requested() {
        let content = "Sodium — 0.6.0\nМод: пример\n";
        let plain = encode_export_text(content, false);
        assert_eq!(plain, content.as_bytes());

        let with_bom = encode_export_text(content, true);
        assert_eq!(&with_bom[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(&with_bom[3..], content.as_bytes());
    }

    #[test]
    fn writes_utf8_text_to_disk() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher-export-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mods.csv");

        write_export_file(&path, "Название,Версия\nМод,1.0\n", true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(
            String::from_utf8(bytes[3..].to_vec()).unwrap(),
            "Название,Версия\nМод,1.0\n"
        );

        std::fs::write(&path, "old").unwrap();
        write_export_file(&path, "plain", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "plain");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
