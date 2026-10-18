use std::path::PathBuf;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid project manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("project schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("not a .livestage bundle: {0}")]
    NotABundle(PathBuf),
    #[error("bundle already exists: {0}")]
    BundleExists(PathBuf),
    #[error("asset path escapes the bundle: {0}")]
    UnsafeAssetPath(String),
    #[error("asset not found: {0}")]
    AssetMissing(String),
    #[error("cue {0} not found")]
    CueNotFound(String),
    #[error("cue error: {0}")]
    Cue(String),
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}
