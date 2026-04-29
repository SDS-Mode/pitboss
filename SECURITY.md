# Security policy

## Supported versions

Pitboss is pre-1.0 and follows the standard pre-1.0 SemVer convention:
breaking changes can land in minor bumps. Only the most recent minor
version receives security fixes. Older versions should upgrade.

| Version | Status |
|---|---|
| 0.9.x | Supported |
| < 0.9 | Unsupported — please upgrade |

## Reporting a vulnerability

**Do not open a public GitHub issue for vulnerabilities.** Use one of
the following private channels:

1. **GitHub Security Advisory** (preferred) — go to the
   [Security tab](https://github.com/SDS-Mode/pitboss/security/advisories/new)
   of this repository and click **Report a vulnerability**. This opens
   a private advisory only the maintainers see, with a built-in
   coordinated-disclosure workflow.
2. **Email** the maintainer at the address listed on the
   [project owner's GitHub profile](https://github.com/SDS-Mode).

Include:

- A description of the vulnerability and the affected component.
- Pitboss version (`pitboss --version`).
- Steps to reproduce, a minimal manifest if applicable.
- Your assessment of impact and severity.
- Whether you'd like to be credited in the advisory.

You should receive an acknowledgement within **5 business days**. If
you don't, please escalate via the alternate channel above (the
acknowledgement may have been lost).

## Disclosure timeline

The default coordinated-disclosure window is **90 days** from the
initial report. We aim to release a fix in less time, but if the issue
is complex or coordination across downstream consumers (Homebrew tap,
container registries) takes longer, we will discuss a timeline
extension with the reporter.

After a fix lands:

1. The advisory is published via GitHub Security Advisories with a CVE
   when applicable.
2. A patch release (`vX.Y.Z+1`) ships through the standard release
   flow (`RELEASE.md`).
3. The CHANGELOG entry under `### Security` references the advisory
   and credits the reporter (if they consented).
4. The Homebrew formula and container images update to the patched
   version automatically.

## Scope

**In scope:**

- The `pitboss`, `pitboss-tui`, and `pitboss-web` binaries and the
  crates they're built from.
- The container images at `ghcr.io/sds-mode/pitboss` and
  `ghcr.io/sds-mode/pitboss-with-claude`.
- The official Homebrew tap formula (`SDS-Mode/homebrew-pitboss`).
- The MCP server's authentication and authorization paths
  (`crates/pitboss-cli/src/mcp/`) and the per-run control socket
  (`crates/pitboss-cli/src/control/`).
- The web console's bearer-token auth and the run filesystem
  read-side endpoints.
- The manifest schema's path-resolution logic (worktree, container
  mount, etc.) — anything that lets a manifest escape its declared
  sandbox.

**Out of scope:**

- Vulnerabilities in third-party dependencies that have not been
  reported upstream first. Please file with the upstream project; we
  will pull the fix once it's released.
- Issues in the `claude` CLI itself — those go to Anthropic.
- Bugs that produce incorrect output but don't compromise integrity,
  confidentiality, or availability — these are normal bugs, file via
  the regular issue tracker.
- Denial of service via budget exhaustion (the operator's manifest
  controls the budget envelope; pitboss enforces it but doesn't
  prevent operators from setting unsafe values).

## Hall of fame

Reporters who responsibly disclose are credited here once the
advisory is public, unless they request anonymity.

*(Empty — be the first.)*
