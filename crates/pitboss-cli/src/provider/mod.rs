pub mod goose;

use std::path::Path;

use pitboss_core::parser::ParseDialect;

pub use goose::{extension_command, shell_word, GooseActorArgs, GooseSpawner};

#[must_use]
pub fn parse_dialect_for_program(program: &Path) -> ParseDialect {
    let name = program
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if name.contains("claude") {
        ParseDialect::Claude
    } else {
        ParseDialect::Goose
    }
}
