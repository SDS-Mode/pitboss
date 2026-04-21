# Release process

Pitboss releases follow a fixed checklist. This document is the canonical
reference — if the steps here disagree with a past release's git history,
trust the history but file a PR updating this doc.

Release mechanics (tarball builds, installers, Homebrew tap publish,
container image tags) are driven by cargo-dist and our three GitHub
Actions workflows. Your job as release driver is the pre-flight prose +
pushing a tag.

---

## When to cut a release

Cut a release when `[Unreleased]` in `CHANGELOG.md` has accumulated any
user-visible behavior change — new feature, bug fix, removal, or breaking
change. Docs-only or CI-only changes alone do not warrant a release.

Use semantic versioning:

- **Major** (`X.0.0`) — breaking changes to the manifest schema, the MCP
  tool surface, the `summary.json` wire format, or CLI subcommand shape.
  Pitboss is still pre-1.0, so breaking changes land in minor bumps until
  the stable-API inflection.
- **Minor** (`0.X.0`) — new features, new MCP tools, new terminal states,
  new subcommands. Default cadence.
- **Patch** (`0.0.X`) — bug fixes and non-breaking doc updates that
  couldn't wait for the next minor.

---

## Pre-flight checklist (run on a `release/vX.Y.Z` branch)

### 1. Version bump — single source of truth

```bash
git checkout -b release/vX.Y.Z main
# Edit Cargo.toml [workspace.package].version → "X.Y.Z"
```

The workspace-root `Cargo.toml` is the only place with a literal version
string. Every crate under `crates/` inherits via `version = { workspace = true }`.

### 2. AGENTS.md frontmatter

Update the YAML frontmatter at the top of `AGENTS.md`:

```yaml
pitboss_version: X.Y.Z
last_updated: YYYY-MM-DD
```

The `pitboss_version` field is how agents decide whether this doc applies
to the binary they're orchestrating — it must match `CARGO_PKG_VERSION` at
release time. The `include_str!` in `crates/pitboss-cli/src/agents_md.rs`
bakes this file into the binary at compile time, so the version baked in
matches the version in the frontmatter.

### 3. CHANGELOG.md — promote `[Unreleased]` to the new version

Replace `## [Unreleased]` with:

```markdown
## [Unreleased]

*(nothing yet)*

## [X.Y.Z] — YYYY-MM-DD

[One-paragraph headline summarizing the release — tagline + the 2-3
highlights most operators will care about. Keep it under 150 words.]

Highlights:
- **[feature]** — one-sentence pitch.
- **[feature]** — one-sentence pitch.
- **[fix]** — one-sentence pitch if it closes a notable gap.

### Added
[... existing Unreleased Added items, unchanged ...]

### Changed
[... existing Unreleased Changed items, unchanged ...]

### Fixed
[... existing Unreleased Fixed items, unchanged ...]

### Docs
[... existing Unreleased Docs items, unchanged ...]
```

The `### Added`/`### Changed`/`### Fixed`/`### Docs` subsections move
verbatim from what was under `[Unreleased]`. Don't re-edit them — they
were written during the feature commits and reflect shipped behavior.

### 4. README.md — replace the version-highlight paragraph

README has one paragraph near the top that describes the current release.
Replace it wholesale with a new paragraph for this version. Reference
`CHANGELOG.md` and `AGENTS.md` as always.

### 5. ROADMAP.md — refresh the "Last refresh" note + adjust section titles

Update the `**Last refresh:**` line at the top:

```markdown
**Last refresh: vX.Y.Z (YYYY-MM-DD).** Everything shipped through
vX.Y.Z has been removed from this file ...
```

If you have a `## Deferred from v<prev>.0 (targeting v<prev+1>+)` section,
rename its title to `## Deferred from vX.Y.Z (targeting v<X.Y.Z+1>+)` and
prune/add items to reflect what actually shipped this release and what
remains deferred.

### 6. book/src/intro.md — bump "Current version" section

Replace the `## Current version` paragraph with a new one for this
release. Shorter than the README paragraph — this renders on the book
landing page, it's operator-facing summary, not release marketing.

### 7. Commit the release prep in one PR

```bash
git add Cargo.toml Cargo.lock AGENTS.md CHANGELOG.md README.md ROADMAP.md book/src/intro.md
git commit -m "release: prep vX.Y.Z"
git push -u origin HEAD
gh pr create --title "release: vX.Y.Z" --body "..."
```

The PR body should link to the `[X.Y.Z]` section of `CHANGELOG.md` on
the branch (a reader reviewing the PR can click through to the full
change list without scrolling the diff).

### 8. CI passes, merge

The release-prep PR runs the full CI matrix. Wait for green, then merge
via `gh pr merge <N> --auto --squash` (or `--admin` if you've decided
branch protection is in the way for this specific case — see the
[branch-protection notes](#branch-protection--merging) below).

---

## Tag and release

Once the release-prep PR is merged to `main`:

```bash
git checkout main
git pull --ff-only
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

This triggers three GitHub Actions workflows in parallel:

### `.github/workflows/release.yml` (cargo-dist)

Auto-generated by `cargo-dist`. Builds platform-specific tarballs +
installers, creates a GitHub Release with the artifacts attached, and
commits an updated formula to the [Homebrew tap](https://github.com/SDS-Mode/homebrew-pitboss).

Duration: ~10-15 minutes. Watch with `gh run watch`.

### `.github/workflows/container.yml`

Builds and pushes multi-arch container images to
`ghcr.io/sds-mode/pitboss:<X.Y.Z>` and
`ghcr.io/sds-mode/pitboss-with-claude:<X.Y.Z>`. Also updates the
`:latest` tag on both images. Includes a post-merge smoke test.

Duration: ~5-7 minutes. Runs in parallel with the release workflow.

### `.github/workflows/book.yml`

Rebuilds the mdBook site and deploys it to GitHub Pages. The updated
`## Current version` section appears on the landing page immediately.

Duration: ~1-2 minutes.

---

## Post-release verification

After all three workflows complete:

### Container image sanity

```bash
podman pull ghcr.io/sds-mode/pitboss-with-claude:X.Y.Z
podman inspect ghcr.io/sds-mode/pitboss-with-claude:X.Y.Z \
  --format '{{index .Config.Labels "ai.anthropic.claude-code.version"}}'
podman run --rm ghcr.io/sds-mode/pitboss-with-claude:X.Y.Z pitboss --version
# → pitboss X.Y.Z
podman run --rm ghcr.io/sds-mode/pitboss-with-claude:X.Y.Z pitboss agents-md | head -5
# → frontmatter with pitboss_version: X.Y.Z
```

### Homebrew tap sanity

```bash
brew tap SDS-Mode/pitboss
brew install pitboss
pitboss --version
# → pitboss X.Y.Z
```

### cargo-dist tarball sanity

```bash
curl -sSfL https://github.com/SDS-Mode/pitboss/releases/download/vX.Y.Z/pitboss-installer.sh | sh
pitboss --version
# → pitboss X.Y.Z
```

### Pages site sanity

Visit <https://sds-mode.github.io/pitboss/> and confirm the "Current
version" callout on the landing page shows `X.Y.Z`.

---

## Branch protection & merging

`main` is protected by a ruleset requiring:

- Pull request (0 approvals — solo-dev repo)
- A single required status check: `All required checks passed` — emitted
  by `.github/workflows/pr-gate.yml`. The gate workflow always runs on
  PRs and aggregates across the other workflows (CI, Container, Book,
  Release) using path filters: it knows which checks *should* have run
  given the PR's changed files, and passes only when those all succeed.
  This way a docs-only PR isn't blocked waiting on the Container
  workflow that was (correctly) path-filtered out.
- Branch up-to-date with main before merge
- Linear history (matches squash-merge)
- No force push, no deletion

To add or remove a required check: edit the `required` array in
`.github/workflows/pr-gate.yml` — **not** the ruleset. The ruleset
itself continues to reference only the gate.

Auto-merge is enabled repo-wide; `delete-branch-on-merge` is on.

**Default merge command:**

```bash
gh pr merge <N> --auto --squash
```

GitHub handles waiting for required checks, keeping the branch up-to-date,
and merging once all conditions clear. No bypass needed.

**Admin bypass** (`--admin`) is available to repository admins and should
be reserved for genuine escape cases:

- Hotfixing a broken `main` where required CI is blocked by the very
  thing the PR fixes
- Protection misconfiguration you're in the middle of fixing
- NOT for "I don't want to wait" — use `--auto` instead

Every admin-bypass merge generates an audit log event. For a solo-dev
repo this is fine, but the audit pattern is useful if collaborators are
added later.

---

## Rolling back a release

Tags on `main` are protected (non-force, non-delete). If a release needs
to be withdrawn:

1. **Don't delete the tag.** Homebrew tap, container images, and
   installed binaries reference it.
2. **Publish a patch release immediately** (`vX.Y.Z+1`) that reverts the
   problematic change.
3. If the release introduced a catastrophic security issue, yank the
   Homebrew formula commit in the tap and mark the GitHub Release as
   "Draft" so casual installs stop picking it up. Existing installed
   binaries continue to work until their next update.
4. Document the rollback rationale in the patch release's CHANGELOG
   entry under `### Security` or `### Changed`.

---

## Changes to this process

If you iterate on the release flow, update this document in the same PR
that lands the change. The goal is that a first-time releaser can follow
this file top-to-bottom and produce a correct release without having to
reconstruct the process from git history.
