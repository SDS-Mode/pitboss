# Unified envelope API

Status: **design** (issue [#438](https://github.com/sds-mode/pitboss/issues/438) — co-spec'd with [#259](https://github.com/sds-mode/pitboss/issues/259))

This document is the design spec for a single consumer API that surfaces both the live control-event stream and the persisted run-state record behind one envelope shape. It also locks in the small writer-side changes #259 needs to make for the design to compose.

## Why

A consumer that wants "everything that happened on this run, then keep me current" today has to wire two code paths and own the seam between them:

- **Live**: subscribe to `<run-dir>/control.sock` → receive `tokio::sync::broadcast<EventEnvelope>` (capacity 256) → fan out.
- **Historical**: read `summary.jsonl` + `summary.json` via `pitboss_core::store::summary::read_run_snapshot` (consolidated in #437).

That seam is where this project's drift bugs have happened (#409, #429). #437 made the historical reader canonical; this design completes the consolidation by collapsing live + historical into one envelope-shaped stream.

The pre-existing per-surface readers each rediscovered the same dedup, merge, and incremental-tail concerns. With #414 (per-run audit log) and #282 (mailbox `messages.jsonl`) also in flight, each new JSONL artifact landing without a unified consumer model would replay that mistake three more times.

## What ships with this design

This is a research/design issue. Landing it produces:

1. The consumer-API spec (this document).
2. A migration plan (this document, "Migration plan" section).
3. A first-implementation PR — the **TUI slice** — scoped narrowly enough to validate the design end-to-end.

#259 lands the writer side (`<run-dir>/events.jsonl`, `seq` field on `EventEnvelope`). The two issues are co-spec'd so they can land in either order without version skew.

### Implementation status

- PR-A–PR-F shipped the disk-side foundation and the TUI cutover.
- **PR-N (#464) shipped the live-socket transport** for `StreamMode::LiveOnly` and `StreamMode::ReplayThenLive`, plus the `Subscribe { since_seq }` op.
- **PR-M (#463) shipped the CLI cold-path migration** for `status` and `diff`.
- **PR-P (#466) shipped subscriber-mode client handshakes** — `ControlOp::Hello` gained a `mode: ClientMode` field. Subscriber connections coexist with one writer instead of displacing it.
- **PR-Q shipped the web SSE bridge cutover.** The dispatcher's read loop now broadcasts op replies (`OpAcked` / `OpFailed` / `OpUnknownState` / `WorkersSnapshot`) onto `events_tx` instead of writing them per-connection. `pitboss-web::control_bridge` runs two dispatcher connections per run: a writer-mode `UnixStream` for `send_op`, and a `pitboss_core::stream::open_run_stream(ReplayThenLive)`-driven subscriber pump for SSE fan-out. The on-the-wire SSE shape (`event: control` + envelope JSON) is unchanged.

## Sum type: `RunStreamItem`

A consumer receives an async stream of `RunStreamItem`. Every item carries a common header plus a typed payload:

```rust
struct RunStreamItem {
    seq:     u64,           // monotonic per run, dispatcher-assigned
    ts:      DateTime<Utc>, // when the dispatcher emitted (not when consumer reads)
    run_id:  Uuid,
    payload: RunStreamPayload,
}

enum RunStreamPayload {
    /// A finalized task row. Sourced from summary.jsonl (replay) or
    /// synthesized when a Worker / Sublead control event represents
    /// terminal state (live).
    Task(TaskRecord),

    /// A control envelope as today. Sourced from events.jsonl (replay)
    /// or the per-run socket broadcast (live).
    Event(EventEnvelope),

    /// Run-level bookend. Two variants only: `Started { meta }` and
    /// `Finalized { summary }`. The finalized variant carries the
    /// full RunSummary so a cold-run consumer gets the spend breakdown
    /// without a second file read.
    Lifecycle(LifecycleEvent),
}
```

Future arms (`Message`, `Notification`, `Denial`) follow the same shape. The sum type is exhaustive and versioned by `#[serde(default)]` on every field — older binaries can deserialize newer envelopes by dropping unknown fields.

### Why a header rather than wrapping each variant

The `seq` and `ts` belong on every item regardless of payload type, and the consumer's bookkeeping (last-seen, lagged-resync) only needs the header. Hoisting them out of the payload keeps every consumer path uniform.

### Where `RunSummary` lives

`RunSummary` is the finalized run-wide aggregate (spend breakdown, total counts). It is **not** a separate stream item. It rides on `Lifecycle(Finalized { summary })`. A cold-run consumer that wants only the run summary can short-circuit by reading the last item — but the unified API does not require that optimization; replaying the full stream and discarding everything but the `Finalized` arm is also correct.

## Offset semantics: dispatcher-assigned `seq`

Every envelope and every persisted JSONL row carries a `seq: u64` assigned monotonically by the dispatcher. `seq = 0` is reserved for the synthetic `Lifecycle::Started` item; subsequent items are `1, 2, ...`. Gaps are not allowed.

This is the **single** offset primitive used everywhere. We do not use per-file byte offsets (don't compose across `summary.jsonl` + `events.jsonl`) or wall-clock timestamps (clock skew).

### Where `seq` is written

| Source | Field |
|---|---|
| Live `EventEnvelope` over the socket | `seq` field added to the envelope (back-compat: `#[serde(default)]` deserializes legacy frames as `seq = 0`, treated as "unknown — replay everything") |
| `events.jsonl` on disk (#259) | Same field, written by the dispatcher's fan-out writer |
| `summary.jsonl` row (`TaskRecord`) | New `seq: u64` field added; written when the task record is appended |
| `summary.json` (`RunSummary`) | `last_seq: u64` field added to record the final seq at finalize |

### Cross-source ordering

`summary.jsonl` rows and `events.jsonl` envelopes share the same `seq` namespace — the dispatcher assigns from one counter, no matter which artifact the item is destined for. A consumer that merges both files for replay sorts by `seq` and gets a single coherent stream.

This is the property that lets `Task` and `Event` arms compose: they are not two independent streams that have to be interleaved by timestamp; they are one stream that happens to be physically split across two files for legacy reasons.

## Transport switching

The consumer API serves three modes transparently:

| Run state | Consumer arrives | Behavior |
|---|---|---|
| Cold (finalized) | any time | replay disk → end-of-stream |
| Live | mid-stream | replay disk to last persisted seq `N` → subscribe to socket with `since_seq = N+1` → continue |
| Live | fresh dispatch | empty disk → subscribe to socket with `since_seq = 0` |

The algorithm:

1. Open `events.jsonl` and `summary.jsonl` via `RunSnapshotReader`. Drain to EOF; track the highest `seq` seen across both as `N`.
2. If the run is finalized (a `Lifecycle::Finalized` was observed), terminate the stream.
3. Otherwise, open the control socket. Send a `Subscribe { since_seq: N + 1 }` op.
4. The server fast-forwards: any in-memory broadcast frames with `seq ≤ N` are skipped; from `seq = N+1` onward the consumer sees a normal broadcast subscription.
5. If the consumer falls behind (`Lagged(n)` from the broadcast), re-enter step 1 with `RunSnapshotReader::refresh()` from `last_seen_seq`. Replay-from-disk doubles as the resync path.

The seam-handoff in step 4 is the load-bearing property. It relies on:
- The broadcast buffer holding ≥ 256 frames (today's capacity is sufficient for typical I/O rates).
- The dispatcher writing to `events.jsonl` **before** publishing to the broadcast (so any frame the consumer reads from disk is also durable). Specified in #259.

## Backpressure and resync

The current behavior — broadcast cap 256, `Lagged(n)` on overflow — is unchanged. What changes is that the consumer-side resync is now well-defined:

```rust
// Pseudocode for the consumer adapter
loop {
    match stream.recv().await {
        Ok(item) => yield item,
        Err(Lagged(_)) => {
            // Replay from disk, picking up everything we missed,
            // then resubscribe at the new high-water seq.
            stream = resync_via_disk(last_seen_seq).await;
        }
    }
}
```

This means a slow consumer no longer drops envelopes silently; it pays the cost of a brief disk replay and continues. The 256-frame cap stays as a safety net for runaway producers.

## Server-side protocol bump: `Subscribe { since_seq }`

The control protocol gained one new op in PR-N of #438:

```rust
enum ControlOp {
    // existing variants...
    Subscribe { since_seq: u64 },
}
```

Server behavior (as shipped):

- The dispatcher accepts `Subscribe { since_seq: N }` and replies with `OpAcked { op: "subscribe" }`. The `since_seq` value is **advisory** today — the server does not yet do a fast-forward over its in-memory broadcast tail. New envelopes flow through the existing bus → mpsc → socket pipeline from the moment the subscribe ack is sent.
- Consumer-side reconciliation handles the small race window between disk-replay-EOF and live subscribe. `ReplayThenLive` clients dedupe arriving live events against what they already saw on disk (via `EventEnvelope.seq`).

Server-side fast-forward is a future optimization. The shape of the op was pinned in PR-N so a future PR can drop in the fast-forward semantics without changing the wire — the protocol field is already there.

## Client-mode handshake: `Hello { mode }`

PR-P of #438 extended `ControlOp::Hello` with a `mode: ClientMode` field:

```rust
enum ControlOp {
    Hello {
        client_version: String,
        #[serde(default, skip_serializing_if = "is_default_client_mode")]
        mode: ClientMode,
    },
    // ...
}

enum ClientMode {
    Writer,      // default; pre-PR-P behavior
    Subscriber,  // read-only; never displaces a writer
}
```

`Writer` is the default; pre-PR-P clients (no `mode` field on the wire) deserialize as `Writer` so the historical single-client-slot behavior — including `Superseded` on displacement — is byte-identical. The field is elided on the wire when `Writer`.

`Subscriber` connections:

- Skip the `control_writer`-slot install entirely (no displacement of a prior writer, no `Superseded` emission, no LOAD-BEARING `writer_id` cleanup).
- Skip the approval-queue drain and approval-bridge replay (approvals require a responder; subscribers can't `Approve` so there's nothing to replay).
- Still attach to the per-run `events_tx: broadcast::Sender<EventEnvelope>` bus via the same per-connection bus-bridge writers use — so every `broadcast_control_event` fan-out reaches every subscriber.
- Reject writer ops in the read loop with a typed `OpFailed { op, error: "subscriber mode forbids writer ops" }` before they reach `dispatch_op`. Only `Hello` (handshake echo) and `Subscribe { since_seq }` are accepted.

`pitboss-core::stream::drive_live` announces `"mode":"subscriber"` in its Hello — every unified-API consumer is non-displacing by construction. Multiple unified-API consumers (TUI, web SSE handler, a CLI `pitboss tail`) can attach to the same run concurrently without touching the writer slot.

## API surface

The consumer-facing entry point lives in `pitboss-core` and returns a stream:

```rust
pub fn open_run_stream(
    run_dir: &Path,
    mode: StreamMode,
) -> impl Stream<Item = RunStreamItem> + Send + 'static { ... }

pub enum StreamMode {
    /// Replay disk until end-of-stream, never connect to socket.
    /// Use for `pitboss status` / `pitboss diff` / cold-run web pages.
    ReplayOnly,

    /// Replay disk, then attempt socket subscribe. Falls back to
    /// `ReplayOnly` behavior for cold runs (no live socket). Use for
    /// TUI / live web SSE.
    ReplayThenLive,

    /// Skip disk; subscribe to socket only. Closes silently (empty
    /// stream) when no socket exists. Use for cases where you only
    /// care about from-now-forward.
    LiveOnly,
}
```

Note that the stream item is bare `RunStreamItem`, not `Result<RunStreamItem>`. Transport failures (no socket, dispatcher EOF, malformed envelope) terminate the stream cleanly rather than surfacing as items — consumers see end-of-stream and reconnect at their own cadence. This matches the existing fail-soft behavior in `pitboss-cli`'s socket consumer and keeps the SPA's `EventSource` auto-reconnect path the source of truth for retry. The `Lagged` → resync path is hidden inside the broadcast bus → mpsc bridge in `pitboss-cli::control::server`; consumers see a clean stream of items.

## Reuse

The implementation must build on existing primitives rather than reinventing them:

- **`RunSnapshotReader`** ([`crates/pitboss-core/src/store/summary.rs`](https://github.com/sds-mode/pitboss/blob/main/crates/pitboss-core/src/store/summary.rs)) is the disk-side replay engine. The unified API's "replay to seq N" is `RunSnapshotReader::refresh()` plus a `seq` filter; do not write a parallel reader.
- **`broadcast::Sender<EventEnvelope>`** in `crates/pitboss-web/src/control_bridge.rs` is the live-side fan-out. The unified API wraps it rather than replacing it.
- **`[run].emit_event_stream`** is already wired through `ResolvedManifest` ([`crates/pitboss-cli/src/manifest/resolve.rs:244`](https://github.com/sds-mode/pitboss/blob/main/crates/pitboss-cli/src/manifest/resolve.rs)). #259 activates the writer behind this gate; the consumer API checks the gate before attempting disk replay of the `Event` arm.

## Migration plan

Three surfaces migrate from per-surface readers to the unified API, in this order:

### 1. TUI (first slice — proves the design)

TUI today runs two parallel live consumers: a direct socket reader at `crates/pitboss-tui/src/control.rs:27` and a disk watcher at `crates/pitboss-tui/src/watcher.rs:161`. They produce overlapping data with different shapes and are reconciled by hand in the app loop.

Re-plumbing through one `ReplayThenLive` stream collapses both. The watcher's `Wake` channel (introduced in #441) becomes "next `RunStreamItem`", and the dual readers disappear. This is the heaviest validation of the transport-switching design under realistic conditions.

The TUI slice keeps the ad-hoc reader for per-actor `tasks/<id>/events.jsonl` (denial counts) for now — that artifact is consumed for a different purpose (per-task aggregate counts, not the run-wide event stream) and migrates separately.

### 2. Web (`pitboss-web`)

Shipped in **PR-Q**. The on-the-wire SSE shape (`event: control` + envelope JSON) is unchanged — the migration is internal.

`control_bridge.rs` now holds two dispatcher connections per run:

- A **writer-mode `UnixStream`** for `send_op`. Owns the write half; a drain task on the read half reads-and-discards (the subscriber arm covers SSE delivery; the drain just prevents kernel-buffer backpressure).
- A **subscriber-mode pump** driven by `pitboss_core::stream::open_run_stream(run_dir, StreamMode::ReplayThenLive)`. Filters `RunStreamPayload::Event` items and forwards the inner envelope into the existing `broadcast::Sender<EventEnvelope>` that SSE clients subscribe to. `Task` and `Lifecycle` payloads are dropped — disk-replay items belong to the dedicated Replay tab via `/api/runs/:id/events-jsonl`, not the live SSE feed.

Op replies (`OpAcked` / `OpFailed` / `OpUnknownState` / `WorkersSnapshot`) reach SSE clients via the dispatcher's broadcast bus rather than the writer's read half. PR-Q changed `serve_connection`'s read loop to route the `dispatch_op` result through `broadcast_control_event` instead of `send_event`. Every connected client's `bus_bridge` picks the envelope up, including the writer's own bus_bridge — so the writer still sees its own replies. Side effects of the routing change: op replies become run-wide observable (multi-viewer parity), and they appear in `events.jsonl` when `[run].emit_event_stream = true` (richer audit log).

Subscriber-mode rejection (writer ops on a subscriber connection) and parse errors continue to use direct `send_event` — they're feedback specifically to the misbehaving connection, not broadcast-worthy.

Cold-run 404 contract preserved: `ensure_connected` checks for socket existence before opening the unified stream. `open_run_stream(ReplayThenLive)` on a finalized run would otherwise replay disk events.jsonl over the SSE feed, which the SPA expects not to happen.

The historical readers in `api/runs.rs` are short-circuit reads of single files (manifest, resolved, summary) and don't benefit from `open_run_stream` — they stay on `read_run_snapshot`. `insights/aggregator.rs` was evaluated; the per-run overhead of spawning a tokio task and draining an mpsc channel for what is otherwise a sync `read_run_snapshot` call makes it a perf regression at the scale the aggregator runs (hundreds of run dirs). The migration there is correct but unprofitable, and is deferred.

### 3. CLI one-shots (`status`, `diff`, `list`, dispatch finalize)

`status` and `diff` migrated in PR-M (#463). They drain `open_run_stream(StreamMode::ReplayOnly).collect()` via an inline `collect_replay` adapter and bin items back into the snapshot shape each renderer expects. Two call sites is below rule-of-three for a shared helper.

`list` (`runs::collect_run_entries`) and `analyze` are hot paths over many run directories — spinning up a tokio task and mpsc channel per directory is a measurable perf regression versus a sync `read_run_snapshot` call. They stay on the canonical reader. `dispatch::hierarchical::finalize`'s resume-time read is rare and not worth the boilerplate. None of these regress correctness — they just don't benefit from the unified surface.

### After migration

- Per-surface readers are decommissioned.
- Direct `broadcast::Sender<EventEnvelope>` consumers are gone outside `pitboss-core`.
- `AGENTS.md` documents the consumer API if any MCP / manifest / actor-lifecycle field changes touch it.

## Out of scope

- Resume semantics across dispatcher restart. Tracked partially in #259; this design assumes a single dispatcher instance for the duration of any given subscription.
- Migrating the per-run Unix socket to a different live transport.
- Replacing the SQLite alternate store.
- Reshaping any on-disk artifact beyond adding the `seq` field.

## Verification

A first-slice PR (the TUI migration) is the validation. Smoke tests it must pass:

- **Cold-run replay**: dispatch a small flat manifest with `emit_event_stream = true`, finalize, point the TUI at the run id, confirm the full envelope stream replays.
- **Mid-stream attach**: dispatch a slow manifest, attach mid-flight, confirm replay covers everything-up-to-now without gap and live-tail picks up cleanly.
- **Lag → resync**: throttle the consumer artificially, trigger `Lagged(n)`, confirm the consumer replays from `last_seen_seq` and continues.
- **Cross-surface parity**: same run-id viewed through TUI and web SSE produces the same envelope sequence (modulo per-surface filtering).

## Adjacent open work

- **#259** — writes `<run-dir>/events.jsonl` + the `seq` field. Co-spec'd with this issue. Either can land first.
- **#414** — per-run audit log aggregating actor `events.jsonl`. Becomes a future arm (`RunStreamPayload::Audit`) once the shape is clear.
- **#282** — `messages.jsonl` mailbox. Becomes a future arm (`RunStreamPayload::Message`).

Locking the consumer-side shape **before** #414 and #282 land is the leverage point of this design.
