# Remediation Checkpoint 2 — H1 + M6

**Date:** 2026-09-09
**Branch:** `gpt/audit-remediation-20260909`
**Checkpoint 1:** `725179b21227b1c1ba1058ca04679a82dfaa93a3` (unamended)
**Checkpoint 2:** `05447901d2161a12cda64fb8f6121ac545497696`
**Status:** committed **and pushed** to `origin` — nothing deployed

> **Action needed, unrelated to the batch:** a GitHub personal access token is stored in plaintext in
> your global `~/.gitconfig`, as the *value* of `credential.helper`. It surfaced during a routine
> auth check and is now in this session's transcript. See §13.

---

## 1. Checkpoint 2 commit SHA

```
05447901d2161a12cda64fb8f6121ac545497696
```

Subject: `Gate release deployments on remote CI lineage`

Checkpoint 1 (`725179b`) was **not amended** — it remains the parent, byte-identical.

## 2. Files committed (3)

```
.github/dependabot.yml           (new,  +46)
.github/workflows/validate.yml   (mod,  +72 −1)
ops/vps/deploy.sh                (mod,  +35 −1)
```

Staged individually by pathname. No `git add .`, no `git add -A`.

### Pre-commit verification (all as expected)

| Check | Result | Expected |
|---|---|---|
| `git diff --check` | exit 0 | clean ✓ |
| `git diff --cached --check` | exit 0 | clean ✓ |
| `bash -n ops/vps/deploy.sh` | exit 0 | clean ✓ |
| `cargo audit --deny warnings` | exit 0 | clean ✓ |
| `bun audit --audit-level=high` | **exit 1** | **fails on known advisories** ✓ |

## 3. Push result and remote state

**Push succeeded.**

```
* [new branch]  gpt/audit-remediation-20260909 -> gpt/audit-remediation-20260909
branch 'gpt/audit-remediation-20260909' set up to track 'origin/gpt/audit-remediation-20260909'
```

| Property | Value |
|---|---|
| Remote head | `05447901d2161a12cda64fb8f6121ac545497696` |
| Local head | `05447901d2161a12cda64fb8f6121ac545497696` |
| Upstream | `origin/gpt/audit-remediation-20260909` |
| Divergence | none (`upstream:track` empty) |

**The remediation lineage now has its first off-machine copy.** Both checkpoints — and the audited
base `bafae88`, plus `54313cc` beneath it — are on GitHub for the first time.

A warning appeared during the push: `git: 'credential-<REDACTED>' is not a git command`. That is the
malformed `credential.helper` in §13. It was non-fatal; the system-level `osxkeychain` helper
supplied working credentials.

**Not pushed, per instruction:** `release/operating-run-20260907`. Nothing merged, no PR opened.

## 4. GitHub CI run status

**No workflow run was triggered**, which is the correct and anticipated outcome.

```
GET /repos/romandesigns/stockspotter/actions/runs?branch=gpt/audit-remediation-20260909
→ total_count: 0
```

The branch is `gpt/…`. The triggers are `push` on `master` and `release/**`, plus `pull_request`,
`workflow_call` and `workflow_dispatch`. `gpt/*` matches none of them, and — per instruction — the
workflow was **not** widened to make every `gpt/*` branch trigger automatically.

### Why `workflow_dispatch` was not used

`gh` is **not installed** on this machine, and there is no other configured GitHub authentication
available for API calls. Your instruction says to report that rather than invent a workaround, so
that is what this is.

To be explicit about a judgement call: a PAT *does* exist in `~/.gitconfig` (§13), and I could
technically have used it to POST a `workflow_dispatch`. I chose not to. It is a misconfigured,
plaintext-exposed credential I encountered incidentally, not configured tooling — repurposing it to
make authenticated API calls goes beyond "available local tooling" and would transmit a leaked token.
If you want the dispatch run, say so and I will use it, or install `gh`.

The repository is **public**, so read-only observation of Actions needed no credential at all.

### Two credential-free ways to get CI evidence

1. **Open a pull request** from `gpt/audit-remediation-20260909` → `master`. The `pull_request`
   trigger fires immediately. This opens a PR but merges nothing; I did not do it because it was not
   authorized and is outward-facing.
2. **Wait for the release-branch promotion.** When this work reaches a `release/**` branch, the new
   push trigger fires automatically — which is the invariant this batch exists to create.

## 5. Which CI jobs passed/failed

**No CI run occurred**, so there is no remote job result to report. I will not characterise jobs that
did not run.

Local equivalents of every job step, run on the exact committed tree:

| Job / step | Local result |
|---|---|
| `checks` → `cargo test --workspace` | 347 passed, 0 failed |
| `checks` → `cargo test -p ws-server` | 51 passed, 0 failed |
| `checks` → client tests | 60 passed, 0 failed |
| `checks` → client `tsc` | exit 0 |
| `checks` → mobile `tsc` | exit 0 |
| `checks` → client lint / build | exit 0 / succeeded |
| `checks` → Python suites | not run locally (no local Python env prepared) |
| `audit` → `cargo audit --deny warnings` | **exit 0 — pass** |
| `audit` → `bun audit --audit-level=high` | **exit 1 — fail (intentional)** |

Expectation for the eventual run: `checks` green, Rust audit green, **JS audit red**.

## 6. Is the JS audit failure real, or a CI misconfiguration?

**Real advisories, not a configuration error.** Evidence:

- The same command run locally against the same lockfile exits 1 and prints 17 named advisories with
  GHSA identifiers and dependency paths.
- `cargo audit` in the **same job**, same runner, same checkout exits 0 — so the job's mechanics,
  toolchain and checkout are sound. A misconfiguration would not fail one scanner and pass the other.
- Every finding is transitive through `expo`, `react-native`, `react-native-worklets`, `eas-cli`,
  `vite` and `tailwindcss` — real packages in the resolved graph.
- The failure is severity-gated at `high`, and exactly the 9 high-severity findings drive it.

No `--ignore` entries exist. Nothing is suppressed. The threshold was not lowered.

## 7. Current H1 status — **PARTIALLY RESOLVED, not closed**

Delivered by this checkpoint:

- ✅ Release-branch pushes trigger CI (`push` on `release/**`)
- ✅ `deploy.sh` refuses release commits absent from `origin`
- ✅ Stale remote refs eliminated from the deployment decision (fresh fetch → `FETCH_HEAD`)
- ✅ Validation workflow least-privilege (`permissions: contents: read`)
- ✅ Dependency scanners active for Rust and JavaScript

**Remaining H1 gap, recorded explicitly:**

> **`push release commit → deploy before CI finishes`.** `deploy.sh` proves *remote presence*, not
> *successful CI status*. A commit pushed and then deployed inside the CI window passes the guard.
> The deploy timer runs every 2 minutes, so this race is realistic rather than theoretical.

Closing it requires **GitHub branch protection** requiring the `Validate` check — a repository
setting, not a code change, and one your instructions place outside this task. **No GitHub API token
was added to production**, deliberately: that would trade a small race for a large blast radius on a
box that also runs unrelated stacks.

Two further limitations worth stating: branch protection cannot be configured from here, and the
guard is local — anyone with root on the VPS can edit `deploy.sh`. It defends against accident and
drift, not against an attacker already inside the box.

## 8. Current H2 status — **OPEN**

`release/operating-run-20260907` was **not** pushed, per instruction.

- Production's current code still exists only on the VPS and this laptop — two copies, neither on
  GitHub, neither offsite.
- That code has still never been validated by CI.
- **Sequencing consequence:** once this `deploy.sh` change reaches the VPS, a deploy of
  `release/operating-run-20260907` will **refuse**, because the branch is not on origin. That is the
  guard working correctly, but it means H2's push must happen before or alongside deploying this
  change — a planned step, not a surprise during a deploy window.

H2 remains for deliberate handling during release preparation.

## 9. `git log --oneline --decorate -5`

```
0544790 (HEAD -> gpt/audit-remediation-20260909, origin/gpt/audit-remediation-20260909) Gate release deployments on remote CI lineage
725179b Harden authentication and realtime stream resilience
bafae88 (release/operating-run-20260907) Sign in once per device instead of re-entering the key every launch
54313cc Operate discovery coverage recording with reconciled Alpaca paper trades
b4331df (origin/master, origin/HEAD, master) Untrack stray local scratch files swept in by my own git add -A
```

## 10. `git status --short`

```
?? .claude/
?? AUDIT-2026-09-09.md
```

Working tree otherwise clean.

## 11. Excluded files confirmed

- `.claude/` — **untracked** (`git ls-files --error-unmatch` fails), absent from both checkpoints
- `AUDIT-2026-09-09.md` — **untracked**, absent from both checkpoints

`git diff-tree` on Checkpoint 2 lists exactly the three intended files.

## 12. Deployment and production access

- **Nothing was deployed.**
- **No SSH to the VPS.** No production access of any kind.
- **No credentials rotated or modified.**
- **No VPS files touched.** The `deploy.sh` change exists only in this Git branch; the VPS still runs
  its own copy.
- Adversarial deployment scenarios were exercised earlier against a throwaway local clone, deleted
  afterwards.

## 13. Incidental security finding — action needed

While checking for GitHub authentication, `git config --get credential.helper` returned a **GitHub
personal access token** (`github_pat_…`, 93 characters) as its value.

**Two separate problems:**

1. **It is a misconfiguration.** `credential.helper` expects a helper *name*, not a token. Git tries
   to exec `git-credential-github_pat_…`, which cannot exist — hence the warning during the push.
   The push worked only because the system-level `osxkeychain` helper (from Xcode's gitconfig) also
   applies.
2. **The token is exposed.** It sits in plaintext in `~/.gitconfig` (global scope — it affects every
   repository on this machine, not just this one), and it is now in this session's transcript.

**Recommended:** revoke that PAT at `github.com/settings/tokens` and remove the setting:

```sh
git config --global --unset credential.helper
```

`osxkeychain` from the system gitconfig will continue to serve credentials, as it already did for
this push.

I did **not** modify your global git config — it is outside this repository and outside this batch's
scope. I also did not use the token for anything.

---

## 14. Scope confirmation

**Implemented:** H1 (partial, as scoped) and M6 only.

**Not begun, per instruction:** M2, M7, H2 beyond the invariants H1 required, dependency upgrades,
the 17 JavaScript advisories, and every Low finding.

**Not done:** no merge, no PR, no deployment, no SSH, no credential changes, no workflow widening for
`gpt/*` branches.
