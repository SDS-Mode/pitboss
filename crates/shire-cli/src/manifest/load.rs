#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::resolve::{resolve, ResolvedManifest};
use super::schema::Manifest;
use super::validate::validate;

/// Load, parse, resolve, and validate a manifest from disk.
pub fn load_manifest(path: &Path, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;
    let mut manifest: Manifest = toml::from_str(&text)
        .with_context(|| format!("parsing manifest at {}", path.display()))?;
    expand_paths(&mut manifest)?;

    let resolved = resolve(manifest, env_max_parallel)?;
    validate(&resolved)?;
    Ok(resolved)
}

fn expand_paths(m: &mut Manifest) -> Result<()> {
    for t in &mut m.tasks {
        t.directory = expand(&t.directory)?;
    }
    if let Some(dir) = &m.run.run_dir {
        m.run.run_dir = Some(expand(dir)?);
    }
    Ok(())
}

fn expand(p: &Path) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    let expanded = shellexpand::full(&s)
        .with_context(|| format!("expanding path {s}"))?;
    Ok(PathBuf::from(expanded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn loads_valid_manifest_from_disk() {
        let dir = TempDir::new().unwrap();
        Command::new("git").args(["init","-q"]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["config","user.email","t@t.x"]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["config","user.name","t"]).current_dir(dir.path()).status().unwrap();
        std::fs::write(dir.path().join("r"), "").unwrap();
        Command::new("git").args(["add","."]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["commit","-q","-m","i"]).current_dir(dir.path()).status().unwrap();

        let manifest = dir.path().join("shire.toml");
        std::fs::write(&manifest, format!(r#"
[[task]]
id = "only"
directory = "{}"
prompt = "hi"
"#, dir.path().display())).unwrap();

        let r = load_manifest(&manifest, None).unwrap();
        assert_eq!(r.tasks[0].id, "only");
    }
}
