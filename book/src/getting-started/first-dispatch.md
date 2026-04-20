# Your first dispatch (flat mode)

Flat mode is the simplest way to use pitboss. You declare N tasks in a TOML manifest; pitboss fans them out in parallel, each in its own git worktree, and collects the results.

## Write a manifest

Create `pitboss.toml`:

```toml
[run]
max_parallel = 2

[[task]]
id = "hello-a"
directory = "/path/to/your/repo"
prompt = "Write 'Hello from worker A' to a file called hello-a.txt at the repo root."
branch = "feat/hello-a"

[[task]]
id = "hello-b"
directory = "/path/to/your/repo"
prompt = "Write 'Hello from worker B' to a file called hello-b.txt at the repo root."
branch = "feat/hello-b"
```

Replace `/path/to/your/repo` with any git repository on your machine. The `branch` fields name the worktree branches pitboss will create. If you omit `branch`, pitboss auto-generates a name.

## Validate first

Always validate before dispatching. This catches schema errors, missing directories, and semantic issues without spawning any `claude` processes:

```bash
pitboss validate pitboss.toml
```

Exit code 0 means the manifest is valid. Non-zero prints the error and exits.

## Dispatch

```bash
pitboss dispatch pitboss.toml
```

Pitboss fans out both tasks in parallel (up to `max_parallel = 2`), streams progress to your terminal, and blocks until all tasks finish.

**Exit codes:**
- `0` — all tasks succeeded
- `1` — one or more tasks failed (pitboss itself ran cleanly)
- `2` — manifest error, missing `claude` binary, etc.
- `130` — interrupted (Ctrl-C; tasks drained gracefully)

## Read the run artifacts

After dispatch, find the run directory:

```bash
RUN_DIR=$(ls -td ~/.local/share/pitboss/runs/*/ | head -1)
echo $RUN_DIR
```

The run directory contains:

| File | Contents |
|------|----------|
| `manifest.snapshot.toml` | Exact manifest bytes used for this run |
| `resolved.json` | Fully resolved manifest (defaults applied) |
| `meta.json` | `run_id`, `started_at`, `claude_version`, `pitboss_version` |
| `summary.json` | Full structured summary written on clean finalize |
| `summary.jsonl` | Appended incrementally as tasks finish |
| `tasks/<id>/stdout.log` | Raw stream-JSON from the task's `claude` subprocess |
| `tasks/<id>/stderr.log` | Stderr output |

Inspect the summary:

```bash
cat "$RUN_DIR/summary.json" | jq '.tasks[] | {id: .task_id, status: .status, tokens: .token_usage}'
```

## Watch the floor (optional)

Start `pitboss-tui` in another terminal while dispatch is running:

```bash
pitboss-tui
```

The TUI opens the most recent run automatically. Press `?` for keybindings. Press `Enter` on a tile to open the Detail view with live log tailing. Press `q` to quit.

## Attach to a single worker

```bash
pitboss attach <run-id> hello-a
```

Follow-mode log viewer for a single task. Run-id is resolved by prefix (first 8 chars of the UUID are enough when unique). Exits on Ctrl-C or when the worker finishes.

## Key manifest knobs for flat mode

| Field | Default | Notes |
|-------|---------|-------|
| `[run].max_parallel` | 4 | How many tasks run concurrently |
| `[run].halt_on_failure` | false | Stop remaining tasks if any task fails |
| `[run].worktree_cleanup` | `"on_success"` | `"always"`, `"on_success"`, `"never"` |
| `[[task]].use_worktree` | true | Set `false` for read-only analysis (no branch needed) |
| `[[task]].timeout_secs` | none | Per-task wall-clock cap |
| `[[task]].model` | see `[defaults]` | Per-task model override |

See [Manifest schema](../operator-guide/manifest-schema.md) for the full field reference.

## Next step

→ [Hierarchical dispatch with a lead](./first-hierarchical.md)
