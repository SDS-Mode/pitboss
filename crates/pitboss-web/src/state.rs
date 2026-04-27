//! Shared application state. Cheap to clone (`Arc` internally for the
//! token + control bridge; paths are owned PathBufs so cheap-ish).

use std::path::PathBuf;
use std::sync::Arc;

use crate::control_bridge::ControlBridge;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
    bridge: ControlBridge,
}

struct Inner {
    runs_dir: PathBuf,
    manifests_dir: PathBuf,
    token: Option<String>,
}

impl AppState {
    pub fn new(runs_dir: PathBuf, manifests_dir: PathBuf, token: Option<String>) -> Self {
        let bridge = ControlBridge::new(runs_dir.clone());
        Self {
            inner: Arc::new(Inner {
                runs_dir,
                manifests_dir,
                token,
            }),
            bridge,
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

    pub fn bridge(&self) -> &ControlBridge {
        &self.bridge
    }
}
