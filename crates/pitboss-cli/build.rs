//! Build script that surfaces the documented macOS `TMPDIR` gotcha as a
//! compile-time `cargo:warning` so contributors who skip `CLAUDE.md` see
//! the issue before their integration tests fail with a mysterious
//! `ENAMETOOLONG` on `bind(2)`.
//!
//! See workspace `CLAUDE.md` ("macOS gotcha: `TMPDIR` and Unix sockets")
//! and issue #543 (F-TEST-9). The `sun_path` limit on macOS is 104 bytes;
//! the default macOS `$TMPDIR` of `/var/folders/AB/<random>/T/` is around
//! 49 chars and overflows once `tempfile` and per-run socket names are
//! appended.

fn main() {
    // Re-run only if `TMPDIR` changes; otherwise cargo caches the build
    // result. Without this, the warning would print on the first build
    // and then never again even if `TMPDIR` was fixed.
    println!("cargo:rerun-if-env-changed=TMPDIR");

    // Read the **target** OS via cargo's `CARGO_CFG_TARGET_OS`. A naive
    // `#[cfg(target_os = "macos")]` inside a build script reflects the
    // *host*, not the target — wrong for cross-compilation (e.g. a Linux
    // host cross-compiling for `aarch64-apple-darwin` would skip the
    // check, and a macOS host cross-compiling for Linux would fire it
    // spuriously). `dist-workspace.toml` cross-compiles to both, so
    // host-gating is incorrect for this project.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        check_tmpdir_length();
    }
}

fn check_tmpdir_length() {
    // The documented workaround `/private/tmp/pb` is 15 chars. The default
    // macOS `/var/folders/...` is ~49 chars. 35 is comfortably above the
    // workaround and below the failure-prone default, so the warning
    // surfaces only when the developer is at risk.
    const SAFE_THRESHOLD: usize = 35;

    let Ok(tmpdir) = std::env::var("TMPDIR") else {
        return;
    };

    if tmpdir.len() > SAFE_THRESHOLD {
        println!(
            "cargo:warning=TMPDIR is {} chars (`{}`) — macOS `sun_path` is \
             only 104 bytes, and pitboss-cli integration tests that bind \
             Unix sockets under TMPDIR will fail with ENAMETOOLONG. \
             Re-run with `TMPDIR=/private/tmp/pb cargo test` or see the \
             workspace CLAUDE.md.",
            tmpdir.len(),
            tmpdir
        );
    }
}
