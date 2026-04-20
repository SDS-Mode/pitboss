//! `pitboss agents-md` — prints the AGENTS.md reference document bundled
//! into this binary at compile time.
//!
//! Use when orchestrating pitboss from an environment without repo access
//! (installed binary, container, CI runner). The embedded content's
//! `pitboss_version` frontmatter is authoritative for the running binary.
//!
//! Parallel delivery path: the container images also copy AGENTS.md to
//! `/usr/share/doc/pitboss/AGENTS.md` for shell-first discovery. Both
//! routes serve the same bytes.

/// Full AGENTS.md content, embedded at compile time from the repo root.
pub const AGENTS_MD: &str = include_str!("../../../AGENTS.md");

/// Write the bundled AGENTS.md document to stdout. No trailing newline
/// is appended — the document already ends in one.
pub fn print_agents_md() {
    print!("{AGENTS_MD}");
}

#[cfg(test)]
mod tests {
    use super::AGENTS_MD;

    #[test]
    fn agents_md_has_frontmatter() {
        assert!(
            AGENTS_MD.starts_with("---\ndocument: pitboss-agent-instructions"),
            "AGENTS.md must open with the documented frontmatter; got first 60 chars: {:?}",
            &AGENTS_MD[..AGENTS_MD.len().min(60)]
        );
    }

    #[test]
    fn agents_md_declares_pitboss_version() {
        assert!(
            AGENTS_MD.contains("\npitboss_version:"),
            "frontmatter must declare pitboss_version"
        );
    }

    #[test]
    fn agents_md_is_not_empty() {
        assert!(AGENTS_MD.len() > 1000, "AGENTS.md should be substantial");
    }
}
