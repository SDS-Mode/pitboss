//! Shared application state. Cheap to clone (Arc internally for the
//! token; paths are owned PathBufs so cheap-ish on clone).

use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    runs_dir: PathBuf,
    manifests_dir: PathBuf,
    token: Option<String>,
}

impl AppState {
    pub fn new(runs_dir: PathBuf, manifests_dir: PathBuf, token: Option<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                runs_dir,
                manifests_dir,
                token,
            }),
        }
    }

    pub fn runs_dir(&self) -> &std::path::Path {
        &self.inner.runs_dir
    }

    #[allow(dead_code)] // Used by Phase 4 manifest endpoints.
    pub fn manifests_dir(&self) -> &std::path::Path {
        &self.inner.manifests_dir
    }

    pub fn token(&self) -> Option<&str> {
        self.inner.token.as_deref()
    }
}
