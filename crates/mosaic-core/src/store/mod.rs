//! Persistence — trait and file-backed implementation.

pub mod record;
pub mod traits;

pub use record::{RunMeta, RunSummary, TaskRecord, TaskStatus};
pub use traits::SessionStore;
