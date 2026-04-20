# Expected Observables — Spotlight #04: Run-global lease contention

This document describes what an operator or reader should expect to observe
when the scenario in `dogfood_run_lease_contention` runs.

## Setup

A root lead spawns two sub-leads:
- **S1** — "sub-lead 1"
- **S2** — "sub-lead 2"

Both S1 and S2 need exclusive write access to a shared operator resource:
`output.json`. This resource lives on the operator's filesystem and must be
protected by a cross-tree lease (not an intra-layer `/leases/*` path).

Initial lease state: `run_leases.snapshot()` shows no active leases for
`output.json`.

---

## Step 1: S1 acquires the lease

**Action:** S1 calls `run_lease_acquire("output.json", ttl_secs=60)`.

**Expected result:** Returns success with `{acquired: true, key: "output.json",
holder: "S1_ID", ttl_secs: 60}`.

**Post-action lease state:** `run_leases.snapshot()` shows one active lease:
- Key: `output.json`
- Current holder: `S1_ID`
- TTL: 60 seconds
- Acquired at: `<timestamp>`

---

## Step 2: S2 attempts to acquire the same lease

**Action:** S2 calls `run_lease_acquire("output.json", ttl_secs=60)`.

**Expected result:** Returns an error indicating the lease is held. The error
message must include S1's actor ID, so S2 (and any operator reading the error)
knows exactly who is blocking them.

Example error: `"cannot acquire output.json: already held by {S1_ID}"`.

**Post-action lease state:** Unchanged. S1 still holds the lease.

---

## Step 3: S1 releases the lease

**Action:** S1 calls `run_lease_release("output.json")`.

**Expected result:** Returns success with `{released: true, key: "output.json"}`.

**Post-action lease state:** `run_leases.snapshot()` no longer contains an entry
for `output.json`. The lease is now available.

---

## Step 4: S2 retries and acquires the lease

**Action:** S2 calls `run_lease_acquire("output.json", ttl_secs=60)` again.

**Expected result:** Returns success with `{acquired: true, key: "output.json",
holder: "S2_ID", ttl_secs: 60}`.

**Post-action lease state:** `run_leases.snapshot()` shows one active lease:
- Key: `output.json`
- Current holder: `S2_ID` (changed from S1)
- TTL: 60 seconds
- Acquired at: `<new timestamp>`

---

## Summary of invariants demonstrated

| Step | Assertion | Expected result |
|---|---|---|
| 1 | S1 acquires → returned `{acquired: true, holder: S1_ID}` | Pass |
| 2 | S2 blocks → returned error containing S1_ID | Pass |
| 2 | Post-block: S1 still holds the lease | Pass |
| 3 | S1 releases → returned `{released: true}` | Pass |
| 3 | Post-release: lease is no longer active | Pass |
| 4 | S2 acquires after release → returned `{acquired: true, holder: S2_ID}` | Pass |
| 4 | Post-reacquire: S2 now holds the lease | Pass |

---

## Guidance from the spec

The pitboss specification distinguishes two coordination primitives:

1. **`/leases/*` (intra-layer):** For coordination within a single layer. Used
   when multiple actors in the same dispatch layer (same shared store) need
   exclusive access to a resource.

2. **`run_lease_*` (run-global):** For coordination across layers/trees. Used
   when resources span sub-tree boundaries, such as:
   - Operator filesystem paths (`/path/to/output.json`)
   - Shared services reachable by all sub-trees
   - Cross-organizational coordination

This spotlight demonstrates the cross-tree case: two sub-leads from different
sub-trees coordinating exclusive access to an operator resource. The run-global
lease API is the right tool for this job.
