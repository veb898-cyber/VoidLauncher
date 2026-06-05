use thiserror::Error;

#[derive(Error, Debug)]
pub enum LauncherError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Version error: {0}")]
    Version(String),

    #[error("Instance error: {0}")]
    Instance(String),

    #[error("Java error: {0}")]
    Java(String),

    #[error("Launch error: {0}")]
    Launch(String),

    #[error("Download error: {0}")]
    Download(String),

    #[error("Mod loader error: {0}")]
    ModLoader(String),
}

impl serde::Serialize for LauncherError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

pub type Result<T> = std::result::Result<T, LauncherError>;
