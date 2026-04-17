//! Scripted fake-claude binary for integration tests.
//!
//! Reads a JSONL script from MOSAIC_FAKE_SCRIPT, executing each action in order:
//!   {"stdout": "..."} — writes line to stdout
//!   {"stderr": "..."} — writes line to stderr
//!   {"sleep_ms": N}   — sleeps for N milliseconds
//!   {"tool_use": {"name": "...", "input": {...}}}
//!       — emits a stream-json assistant tool_use event on stdout
//!   {"mcp_call": {"name": "...", "args": {...}, "bind": "...", "allow_err": bool}}
//!       — issues an MCP tool call (requires PITBOSS_FAKE_MCP_SOCKET).
//!
//! After the script, exits with MOSAIC_FAKE_EXIT_CODE (default 0).
//! If MOSAIC_FAKE_HOLD=1, blocks indefinitely after the script (for Ctrl-C tests).
//! Special-cases --version to print "fake-claude 0.0.0".

mod bindings;
mod mcp_client;
mod script;

use std::io::BufReader;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --version flag.
    if args.iter().any(|a| a == "--version") {
        println!("fake-claude 0.0.0");
        return;
    }

    let exit_code: i32 = std::env::var("MOSAIC_FAKE_EXIT_CODE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let hold = std::env::var("MOSAIC_FAKE_HOLD")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);

    // Execute the script if provided.
    if let Ok(script_path) = std::env::var("MOSAIC_FAKE_SCRIPT") {
        let file = std::fs::File::open(&script_path)
            .unwrap_or_else(|e| panic!("fake-claude: cannot open script {script_path:?}: {e}"));
        let reader = BufReader::new(file);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");

        // Open an MCP client if the socket env var is set. Unset => None.
        let client = match std::env::var("PITBOSS_FAKE_MCP_SOCKET") {
            Ok(sock) => {
                let path = std::path::PathBuf::from(sock);
                match rt.block_on(mcp_client::McpClient::connect(&path)) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        eprintln!("fake-claude: mcp connect {}: {e:#}", path.display());
                        std::process::exit(2);
                    }
                }
            }
            Err(_) => None,
        };

        if let Err(e) = rt.block_on(script::execute_script(reader, client)) {
            let msg = format!("{e:#}");
            eprintln!("fake-claude: script error: {msg}");
            // Distinguish exit codes by error type (see spec §Error handling).
            let code = if msg.contains("unknown binding")
                || msg.contains("no path")
                || msg.contains("substitute")
            {
                4
            } else if msg.contains("requires PITBOSS_FAKE_MCP_SOCKET") {
                3
            } else {
                5
            };
            std::process::exit(code);
        }
    }

    if hold {
        // Block indefinitely — wait for SIGINT/SIGTERM from the test harness.
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    std::process::exit(exit_code);
}
