//! Regression test for #427 / #428 (F-TEST-4): the connect-timeout
//! call in `app.rs::run` must construct `tokio::time::timeout(...)`
//! *inside* the runtime context.
//!
//! `tokio::time::timeout(...)` registers with the timer driver at
//! **construction**, not at first poll. If constructed outside a
//! runtime (e.g. as the argument to `runtime.block_on(...)` directly),
//! it panics with `"there is no reactor running, must be called from
//! the context of a Tokio 1.x runtime"`. Pre-fix, every `pitboss-tui`
//! launch hit this on the connect-control path, making v0.13.0
//! unusable.
//!
//! The fix wraps the timeout in `async { ... .await }` so the timeout
//! is constructed inside the future being polled by `block_on`. This
//! test mirrors the exact pattern from `app.rs:90-102`. A regression
//! that unwraps the async block as a "simplification" would panic in
//! the body of this test.

use std::path::PathBuf;
use std::time::Duration;

#[test]
fn timeout_constructed_inside_async_block_does_not_panic() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Exact production pattern from `pitboss_tui::app::run`. The future
    // passed to `block_on` constructs `tokio::time::timeout` AFTER the
    // runtime context is entered. `open_run_session` returns a
    // `RunStreamSession` directly (not a `Result`), so the outer
    // `Result` is purely the timeout's elapsed-vs-completed flag.
    let result: Result<pitboss_cli::live_stream::RunStreamSession, tokio::time::error::Elapsed> =
        runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(50),
                pitboss_cli::live_stream::open_run_session(
                    PathBuf::from("/tmp/pitboss-tui-tests-nonexistent-run-dir"),
                    Some(PathBuf::from(
                        "/tmp/pitboss-tui-tests-nonexistent-control.sock",
                    )),
                    pitboss_cli::live_stream::LiveStreamMode::LiveOnly,
                ),
            )
            .await
        });

    // The outcome doesn't matter for this regression test — what we're
    // proving is that `tokio::time::timeout` construction inside the
    // async block doesn't panic with "no reactor running". A missing
    // run dir / socket either completes fast (Ok) or elapses (Err)
    // within the 50ms budget; both are acceptable.
    let _ = result;
}

#[test]
fn naive_block_on_pattern_panics_when_constructed_outside_runtime() {
    // Negative coverage: prove the original v0.13.0 pattern panics. If
    // someone "simplifies" the wrapper away again, this test pins what
    // the failure mode actually was — a panic in the runtime layer.
    //
    // We catch the panic with `std::panic::catch_unwind` so this test
    // reports the regression rather than crashing the harness. The
    // closure constructs `tokio::time::timeout` *outside* an `async`
    // block — the same shape that broke v0.13.0.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // `catch_unwind` requires `UnwindSafe`; both the runtime and the
    // pure tokio future satisfy this for the construction-time panic
    // we're catching.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(tokio::time::timeout(
            Duration::from_millis(50),
            pitboss_cli::live_stream::open_run_session(
                PathBuf::from("/tmp/pitboss-tui-tests-nonexistent-run-dir"),
                Some(PathBuf::from(
                    "/tmp/pitboss-tui-tests-nonexistent-control.sock",
                )),
                pitboss_cli::live_stream::LiveStreamMode::LiveOnly,
            ),
        ))
    }))
    .is_err();

    assert!(
        panicked,
        "tokio::time::timeout constructed outside async block should panic \
         with 'no reactor running' — if this assertion fails, either tokio's \
         behavior changed or the test is no longer exercising the pre-fix \
         shape that v0.13.0 hit."
    );
}
