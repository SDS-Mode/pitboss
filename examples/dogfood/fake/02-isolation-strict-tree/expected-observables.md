# Expected Observables — Spotlight #02: Strict-tree isolation

This document describes what an operator or reader should expect to observe
when the scenario in `dogfood_isolation_strict_tree` runs.

## Setup

A root lead spawns two sub-leads in parallel:
- **S1** — "phase 1: gather inputs"
- **S2** — "phase 2: process outputs"

Each sub-lead has its own isolated KvStore layer. Root has its own layer too.

---

## KV Isolation (per-layer stores)

### S1 writes `/shared/progress`
- S1 calls `kv_set("/shared/progress", "phase 1 complete")`
- This write lands in **S1's layer store** only.
- S2's layer store is unaffected.
- Root's layer store is unaffected.

### S2 writes `/shared/progress`
- S2 calls `kv_set("/shared/progress", "phase 2 in progress")`
- This write lands in **S2's layer store** only.
- S1's layer store is unaffected.
- Root's layer store is unaffected.

### S1 reads `/shared/progress`
- Returns **"phase 1 complete"** — its own write, not S2's.

### S2 reads `/shared/progress`
- Returns **"phase 2 in progress"** — its own write, not S1's.

### Root reads `/shared/progress`
- Returns **`{"entry": null}`** — root's layer has no write to this path.

---

## Strict Peer Visibility (same-layer workers)

Two root-layer workers (W1 and W2) demonstrate the peer-visibility rule.
Sub-leads are NOT the right actors for this demonstration because each
sub-lead is the lead of its own layer, meaning it has full visibility over
its own layer's `/peer/` namespace. Peer visibility is enforced between
actors within the *same* layer.

### W1 writes its own peer slot
- W1 calls `kv_set("/peer/self/status", "halfway")`
- This resolves to `/peer/W1/status` in root's layer store.

### W2 tries to read W1's peer slot
- W2 calls `kv_get("/peer/W1/status")`
- **Result: error** — "strict peer visibility: W2 cannot read /peer/W1/*;
  only W1 itself or the layer lead (root) may read this slot"

### Root lead reads W1's peer slot
- Root calls `kv_get("/peer/W1/status")`
- **Result: success** — root is the layer lead, so it has full visibility.
- Returns the entry written by W1.

---

## Summary of invariants demonstrated

| Scenario | Expected result |
|---|---|
| S1 reads its own `/shared/progress` | Returns S1's write |
| S2 reads its own `/shared/progress` | Returns S2's write |
| Root reads `/shared/progress` | Returns `null` (no root write) |
| W2 reads W1's `/peer/W1/status` | **Error** (strict peer visibility) |
| Root reads W1's `/peer/W1/status` | Success (layer-lead privilege) |
