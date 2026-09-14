# Remediation Batch 3 — H1 (CI bypass) + M6 (dependency scanning)

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Base:** checkpoint `725179b`
**Status:** implemented and validated — **not committed, not staged, not pushed**

> **Headline:** Rust dependencies are clean. The JavaScript graph carries **17 real advisories
> (9 high, 5 moderate, 3 low)**, so the JS audit gate **fails today**. It was not weakened to
> produce a green build. See §10 and the decision you need to make in §18.

---

## 1. Architectural findings from inspection

### 1.1 The audit's description of existing CI was accurate

`validate.yml` genuinely runs Rust tests (`cargo test --workspace --locked`), client tests, client
lint, client build, mobile typecheck, and **two** Python suites (`pytest` plus a `unittest discover`
for `test_discovery*.py`). Nothing was overstated or understated. All of it is preserved verbatim.

### 1.2 There is a second workflow, and it depends on this one

`.github/workflows/desktop-release.yml` builds and signs the Windows installer and **calls
`validate.yml` via `uses:`** as a gate before releasing. That is why `workflow_call` already exists
as a trigger, and it constrains how `validate.yml` may be changed — its job structure and reusability
had to be preserved.

### 1.3 `validate.yml` had **no `permissions:` block at all**

It therefore inherited the repository default, which for many repositories is read-write. A workflow
that only checks out and tests should never hold a writable token, and this one also runs on
`pull_request` — including from forks. This was not in the audit; it is a new finding.

### 1.4 The release-branch convention is `release/*`, from `deploy.sh` itself

`ops/vps/deploy.sh` accepts exactly `master` and `release/*`, refusing everything else. So
`release/**` is the correct Actions glob — derived from the deployment script rather than assumed.

### 1.5 `bun audit` exists natively and is the right tool here

Bun 1.3.13 ships `bun audit` with `--json`, `--audit-level` and `--ignore`. The audit's suggestion of
`npm audit` was wrong for this repository — it cannot run against a Bun workspace without generating
a `package-lock.json`. No npm involvement is needed or used.

### 1.6 No Dependabot or Renovate configuration existed

Confirmed absent. No `cargo-audit`, `cargo-deny` or RustSec reference anywhere in the repository
either.

---

## 2. CI trigger design

```yaml
on:
  push:
    branches:
      - master
      - "release/**"
  pull_request:
  workflow_call:        # preserved — desktop-release.yml depends on it
  workflow_dispatch:    # preserved — operational reruns
```

`release/**` is the fix for H1's CI half. Scratch branches are deliberately **not** matched: they
reach production only by merging to `master` or being promoted to a release branch, both covered, so
validating every throwaway branch would burn CI minutes without adding safety.

`workflow_dispatch` was already present and is retained — but per your instruction it is explicitly
**not** treated as sufficient, because it cannot prevent a bypass on its own. The deployment guard in
§3 is what makes bypass hard rather than merely possible-to-detect.

---

## 3. Deployment guard design

Added to the `release/*` case in `ops/vps/deploy.sh`:

```sh
if ! git fetch --quiet origin "$BRANCH" 2>/dev/null; then
  echo "Deployment refused: $BRANCH has no counterpart on origin (push it first)" >&2
  exit 1
fi
if ! git merge-base --is-ancestor HEAD FETCH_HEAD; then
  echo "Deployment refused: HEAD is not present on origin/$BRANCH" >&2
  echo "  local HEAD:      $(git rev-parse HEAD)" >&2
  echo "  origin/$BRANCH: $(git rev-parse FETCH_HEAD)" >&2
  exit 1
fi
```

The invariant: **a commit that has never been pushed to origin cannot be deployed as production
release code.** That is precisely the historical failure mode — the deployed release branch existed
only in the VPS checkout, unpushed, unvalidated and unbacked-up.

**Deliberately not implemented:** querying GitHub Actions status from the VPS. That would require
giving a production box a GitHub API token — a materially larger blast radius than the property it
buys. Presence-on-origin is a pure Git invariant with no credentials, and branch protection on the
GitHub side is the right place to require green CI.

**Honest limitation:** this proves *presence on origin*, not *CI success*. A commit pushed and then
deployed before CI finishes would pass the guard. Closing that needs branch protection or the API
token above — see §13.

The `master` path is unchanged (`fetch` + `merge --ff-only`), as are the dirty-checkout refusal, the
`flock` lock, the state file, and every post-deploy health gate.

---

## 4. Stale remote refs

The guard never trusts a cached `refs/remotes/origin/...` ref. It runs `git fetch origin "$BRANCH"`
immediately before the check and compares against `FETCH_HEAD` — the value just retrieved. This
matters concretely: the VPS previously only ever fetched `master`, so any cached release ref could be
arbitrarily stale.

**Tested adversarially.** I forged `refs/remotes/origin/release/test` to point at an unpushed local
commit — i.e. the cached ref lied and claimed origin already had it. The guard still refused, because
it consulted the fresh fetch:

```
forged origin/release/test -> 7684037 (a lie)
origin actually has        -> 6d19218
Deployment refused: HEAD is not present on origin/release/test
exit: 1
```

---

## 5. Rust dependency-scanning design and versioning

```yaml
- name: Install cargo-audit
  run: cargo install cargo-audit --locked --version 0.22.2
- name: Rust advisories (RustSec)
  run: cargo audit --deny warnings
```

**Versioning choice.** The *tool* is pinned to 0.22.2 — the version verified locally — so a
cargo-audit release cannot change CI behaviour on its own, and `--locked` pins its own dependency
tree. The *advisory database* is deliberately **not** pinned: a newly published advisory against an
unchanged dependency tree is exactly the signal this job exists to surface, and freezing it would
defeat the purpose.

**Why `cargo install` rather than an action.** It adds no third-party action dependency and is fully
deterministic. The trade is build time (~2–4 min), mitigated by running in a parallel job with
`Swatinem/rust-cache`. `rustsec/audit-check` was considered and rejected: it wants a `GITHUB_TOKEN`
and issue-write permission to file findings, which conflicts with the least-privilege posture in §8.

**`--deny warnings`** also fails on unmaintained and yanked crates, not just vulnerabilities. Safe to
enable because the tree is currently clean at that strictness.

---

## 6. JavaScript/Bun dependency-scanning design

```yaml
- run: bun install --frozen-lockfile
- name: JavaScript advisories
  run: bun audit --audit-level=high
```

Bun's **native** audit against `bun.lock`. No `package-lock.json` generated, no `npm install`, no
package-manager switch, no lockfile replacement — all explicitly avoided per instruction.

**Threshold at `high`.** Moderate and low findings still print in the log but do not fail the build.
This is a severity gate, not suppression: no `--ignore` list is used, and nothing is hidden.

**Documented limitation:** `bun audit` reports advisories against the resolved dependency graph but
does not, on its own, distinguish dev-only from runtime dependencies. Several findings here are in
build tooling (`vite`, `tailwindcss`, `eas-cli`) rather than shipped runtime code — relevant to how
urgently they need fixing, but not something the tool separates for us.

---

## 7. Dependabot — added

`.github/dependabot.yml`, three ecosystems, weekly, grouped, with low PR limits (5/5/3) to keep a
single-operator project from drowning in noise:

| Ecosystem | Key | Rationale |
|---|---|---|
| Rust | `cargo` | Currently clean; this keeps it that way |
| JavaScript | `bun` | First-class Dependabot ecosystem (Bun ≥ 1.1.39; repo pins 1.4.0 for EAS) |
| Workflow actions | `github-actions` | A compromised action is a supply-chain path straight into CI |

I verified Bun ecosystem support against GitHub's documentation rather than assuming it — the audit's
`npm audit` suggestion had already proved wrong for this repo.

**Stated limitation, not glossed:** GitHub lists the `bun` ecosystem for **version updates**.
Security-update coverage is not something this file can assert, so `bun audit --audit-level=high` in
CI remains the authoritative JS advisory gate rather than a backstop. Scanning and update automation
are complementary, not substitutes.

---

## 8. Workflow permissions assessment

**Finding (new, not in the audit):** `validate.yml` had no `permissions:` block, inheriting the
repository default — potentially read-write, on a workflow that also runs `pull_request` from forks.

**Fixed**, as it is obviously safe and directly related to CI hardening:

```yaml
permissions:
  contents: read
```

Verified this does not break anything: `validate.yml` references **no** `secrets.*` (the only grep
match is my own comment). `desktop-release.yml` uses secrets in 3 places and declares its own
`contents: write` on the release job, so the reusable-workflow call is unaffected.

`desktop-release.yml` itself has no workflow-level `permissions` block. Its release job correctly
scopes `contents: write`, but a top-level default-deny there would be tighter. **Not changed** — it
is beyond this batch's scope and not obviously risk-free, since a Tauri release job's needs are
easier to break than to verify. Reported for a later batch.

---

## 9. Files changed

```
 M .github/workflows/validate.yml   (+72 −1)
 M ops/vps/deploy.sh                (+35 −1)
?? .github/dependabot.yml           (new)
```

No source code was modified. `bun.lock`, `Cargo.toml`, `package.json` and every crate are untouched.

---

## 10. Scanner results

### Rust — clean

```
cargo-audit 0.22.2
Loaded 1243 security advisories
Scanning Cargo.lock for vulnerabilities (216 crate dependencies)
cargo audit                  exit 0
cargo audit --deny warnings  exit 0
```

Zero vulnerabilities, zero unmaintained crates, zero yanked crates.

### JavaScript — 17 advisories, all transitive

`bun audit --audit-level=high` **exits 1**. Full enumeration:

| Severity | Package | Advisory |
|---|---|---|
| high | `image-size` | ICNS parser DoS via infinite loop (GHSA-w3rx-r6r6-pgpr) |
| high | `image-size` | JXL/HEIF parser DoS via infinite loops (GHSA-5p2g-fcmc-qvqq) |
| high | `minimatch` | ReDoS via repeated wildcards with non-matching input |
| high | `minimatch` | ReDoS: `matchOne()` combinatorial backtracking |
| high | `minimatch` | ReDoS: nested `*()` extglobs |
| high | `nanoid` | Non-secure generators loop indefinitely on negative size |
| high | `nanoid` | Custom generators loop indefinitely |
| high | `nanoid` | Integer overflow / wraparound |
| high | `node-tar` | Uncontrolled recursion in `mapHas`/`filesFilter` |
| moderate | `ajv` | ReDoS with the `$data` option |
| moderate | `joi` | Uncaught `RangeError` on deeply nested input |
| moderate | `ts-deepmerge` | Prototype method override → DoS |
| moderate | `uuid` | Missing buffer bounds check in v3/v5/v6 |
| moderate | `yaml` | Stack overflow via deeply nested collections (GHSA-48c2-rrv3-qjmp) |
| low | `jsdiff` | DoS in `parsePatch` |
| low | `joi` | Prototype pollution via `__proto__` language key |
| low | `joi` | `object().rename()` template target |

**Every one is transitive**, reached through `expo`, `react-native`, `react-native-worklets`,
`eas-cli`, `vite` and `tailwindcss`. None is a direct dependency of this project.

**Remediation belongs in a separate batch, not this one.** Fixing them means moving Expo /
React Native / Vite major versions — a dependency-upgrade batch with its own mobile and client
regression testing. Your instructions explicitly forbid broad dependency upgrades here, and
auto-upgrading that tree to chase a green build would be exactly the wrong move.

The scanner was **not** weakened to hide this. No `--ignore` entries exist.

---

## 11. Validation results

| Check | Result |
|---|---|
| `bash -n ops/vps/deploy.sh` | **exit 0** |
| `shellcheck` | **not installed** — not installing a toolchain solely for it, per instruction |
| YAML parse — `validate.yml` | **OK**: triggers `push, pull_request, workflow_call, workflow_dispatch`; push branches `["master","release/**"]`; permissions `{"contents":"read"}`; jobs `checks, audit` |
| YAML parse — `desktop-release.yml` | **OK**, unchanged, still calls `validate.yml` |
| YAML parse — `dependabot.yml` | **OK**: ecosystems `cargo, bun, github-actions` |
| `cargo audit --deny warnings` | **exit 0** |
| `bun audit --audit-level=high` | **exit 1** (real advisories — see §10) |
| Project test suites | **Not re-run.** No source, lockfile or manifest changed; the checkpoint's results (51 ws-server / 347 workspace / 60 client / both `tsc` clean) still stand. |

YAML was parsed with the real `yaml` parser already present in `node_modules`, not by eyeballing —
no new dependency was installed for it.

---

## 12. Adversarial scenarios A–F

All deployment scenarios were executed against a **throwaway clone** (bare `origin.git` + working
clone, everything after the branch guard stubbed out), never the real deployment. The harness was
deleted afterwards.

| # | Scenario | Expected | Actual |
|---|---|---|---|
| — | Baseline: pushed release commit | proceed | `GUARD PASSED`, **exit 0** ✓ |
| **A** | Local commit never pushed | refuse | `Deployment refused: HEAD is not present on origin/release/test`, **exit 1** ✓ |
| **B** | Local branch ahead of origin | refuse | same refusal, **exit 1** ✓ |
| **C** | Local branch behind origin | documented | **allowed, exit 0** — deploys the checked-out commit. See below. |
| **D** | Release branch pushed | CI triggers | Verified by config: `push.branches` includes `release/**` ✓ |
| **E** | PR from untrusted fork | no secrets, no write | `permissions: contents: read`; **zero** `secrets.*` references in `validate.yml` ✓ |
| **F** | Scanner finds a real advisory | fail clearly | `bun audit --audit-level=high` **exit 1**; no suppression ✓ |

Two extra cases I added:

| Scenario | Result |
|---|---|
| Forged/stale `origin/release/*` ref | **Refused** — guard uses the fresh fetch, not the cached ref ✓ |
| Release branch with no counterpart on origin | **Refused**: `has no counterpart on origin (push it first)` ✓ |

**Scenario C reasoning.** A behind-origin checkout is *allowed* and deploys what is checked out. I
deliberately did not add auto-fast-forward for release branches: they are advanced by an operator on
purpose, so silently pulling them forward would deploy code nobody chose to promote — the opposite of
the property this guard creates. The commit is still provably on origin, so the H1 invariant holds.
`master` retains its existing `merge --ff-only`.

**Harness note:** `flock` does not exist on macOS, so `flock -n 9 || exit 0` made the script exit 0
before reaching the guard. That is a macOS artifact — the VPS is Ubuntu, where `flock` is present —
and I stubbed it out in the harness rather than changing the script.

---

## 13. Remaining H1 limitations

1. **Presence on origin ≠ CI passed.** A commit pushed and deployed within the CI window would pass
   the guard. Closing this needs GitHub branch protection requiring the `Validate` check, or a
   GitHub API token on the VPS — the latter deliberately rejected as too large a blast radius.
2. **Branch protection cannot be configured from here.** It is a GitHub repository setting, and your
   instruction forbids configuring it through external APIs. It remains a manual step.
3. **The deploy timer runs every 2 minutes**, so a push-then-immediately-deploy race is realistic
   rather than theoretical. Branch protection is the real mitigation.
4. **The guard is local.** Anyone with root on the VPS can edit `deploy.sh`. It defends against
   accident and drift, not against an attacker already inside the box.

---

## 14. What remains unresolved for H2

**H2 is not resolved, and I want to be unambiguous about that.**

This batch establishes the *expectation* that deployable release branches are pushed to `origin` —
and now enforces it at deploy time. But:

- **Nothing has been pushed.** `release/operating-run-20260907` still exists only on the VPS and this
  laptop. Two copies, neither offsite, neither on GitHub.
- **Consequently, production's current code still has no CI validation and no durable backup.** The
  workflow is *ready* to validate release branches; it has not validated this one.
- **The guard is not yet satisfiable for the current production branch.** If `deploy.sh` with this
  change were deployed today, the next deploy of `release/operating-run-20260907` would **refuse**,
  because that branch is not on origin. That is the correct behaviour and exactly the point — but it
  means H2's push must happen before or alongside deploying this change.

What H2 still needs: pushing release branches to `origin`, and deciding whether they are protected,
private, or retention-managed. None of it is done here.

---

## 15. `git diff --stat`

```
 .github/workflows/validate.yml | 72 +++++++++++++++++++++++++++++++++++++++++-
 ops/vps/deploy.sh              | 35 +++++++++++++++++++-
 2 files changed, 105 insertions(+), 2 deletions(-)
```

## 16. `git status --short`

```
 M .github/workflows/validate.yml
 M ops/vps/deploy.sh
?? .claude/
?? .github/dependabot.yml
?? AUDIT-2026-09-09.md
```

`.claude/` and `AUDIT-2026-09-09.md` untouched and unstaged. `.github/dependabot.yml` is the new
file this batch adds; it would need explicit staging when a commit is approved. Nothing staged,
committed or pushed.

---

## 17. Important changed sections

### `.github/workflows/validate.yml`

```yaml
on:
  push:
    branches:
      - master
      - "release/**"        # ops/vps/deploy.sh deploys from these
  pull_request:
  workflow_call:
  workflow_dispatch:

permissions:
  contents: read            # was: unset, inheriting the repo default

jobs:
  checks:                   # every existing step preserved verbatim
    ...
  audit:                    # new, separate job so the signals stay readable
    steps:
      - run: cargo install cargo-audit --locked --version 0.22.2
      - run: cargo audit --deny warnings
      - run: bun install --frozen-lockfile
      - run: bun audit --audit-level=high
```

### `ops/vps/deploy.sh`

```sh
release/*)
  # A release commit must exist on origin before it can be production.
  # Fetched fresh into FETCH_HEAD rather than trusting a cached
  # origin/<branch> ref, which can be arbitrarily stale on a box that only
  # ever fetches master.
  if ! git fetch --quiet origin "$BRANCH" 2>/dev/null; then
    echo "Deployment refused: $BRANCH has no counterpart on origin (push it first)" >&2
    exit 1
  fi
  if ! git merge-base --is-ancestor HEAD FETCH_HEAD; then
    echo "Deployment refused: HEAD is not present on origin/$BRANCH" >&2
    echo "  local HEAD:      $(git rev-parse HEAD)" >&2
    echo "  origin/$BRANCH: $(git rev-parse FETCH_HEAD)" >&2
    exit 1
  fi
  # Behind origin is allowed and deploys what is checked out -- release
  # branches are advanced deliberately, so silently fast-forwarding would
  # deploy code nobody chose to promote.
  ;;
```

---

## 18. Recommendation

**Ready to commit — but one decision is yours first, because it determines what "green CI" means
from here.**

The `bun audit --audit-level=high` gate **fails today** against 17 real advisories. Three options:

1. **Commit as-is.** CI goes red on the `audit` job immediately, honestly reflecting that the JS
   graph carries 9 high-severity advisories. The `checks` job stays green and legible on its own,
   since they are separate jobs. Red until a dependency-upgrade batch lands.
2. **Commit with a documented, time-boxed `--ignore` list** naming each advisory and the reason.
   Visible and reviewable rather than silent — but it is still a suppression, and suppressions
   outlive their justifications.
3. **Lower the JS gate to `critical`** so today's `high` findings report without failing. The
   weakest option; I would not choose it.

**I recommend option 1**, with a dependency-upgrade batch scheduled next. Your own instruction —
*"do not silently ignore vulnerabilities just to obtain a green build"* — points the same way, and a
red job that names nine real advisories is more useful than a green one that hides them.

One sequencing point regardless of choice: **H2's push must happen before this `deploy.sh` change
reaches the VPS**, or the next production deploy will refuse. That refusal would be correct, but it
should be a planned step rather than a surprise.

Nothing committed. No deployment, no SSH, no credential changes, no push.
