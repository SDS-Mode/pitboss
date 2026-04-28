//! Typed manifest errors. Currently scoped to snapshot version
//! incompatibility — the rest of the manifest pipeline still uses
//! `anyhow::Error` for terse one-off bails.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    /// The snapshot on disk was produced by a newer pitboss release that
    /// declared a higher `manifest_schema_version` than this binary supports.
    /// Older snapshots (lower version, including missing → treated as `0`)
    /// are accepted on a best-effort basis and never raise this error.
    #[error(
        "incompatible manifest schema in snapshot: snapshot is v{found}, this pitboss supports up to v{supported}. \
         The run was likely produced by a newer pitboss release; upgrade pitboss or re-run from scratch."
    )]
    IncompatibleVersion { found: u32, supported: u32 },
}
