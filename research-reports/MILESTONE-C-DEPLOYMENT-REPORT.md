# ALPHA MILESTONE C — DEPLOYMENT AND LIVE VERIFICATION

**Date:** 2026-09-09
**Status:** §§1–7 **complete and verified**. §§8–10 (session capture, export, local transfer)
**pending tomorrow's market open** — today's regular session closed at 16:00 EDT before this milestone
began.

Per §12: *"If the complete regular session cannot be captured today because the market session has
already ended, complete the release/deployment and live measurement verification now, then capture
the next complete regular session without changing the code or strategy in between."* That is exactly
what this report covers. **No partial session is being presented as complete.**

---

## 1. Release branch state

Fast-forwarded and pushed. Linear, no rebase, no squash, no merge commit, no force:

```
bafae88  Sign in once per device…              (audited base)
725179b  Harden authentication…                 checkpoint 1
0544790  Gate release deployments…              checkpoint 2
ba72269  Build causal alpha measurement pipeline
```

`git log --merges bafae88..HEAD` is empty — verified linear.

**No remote release branch existed beforehand**, so there was no divergence risk and nothing was
overwritten. Had one existed at a conflicting commit I would have stopped.

## 2. Remote / push state

| Branch | Remote head |
|---|---|
| `gpt/audit-remediation-20260909` | `ba722698…` |
| `release/operating-run-20260907` | `ba722698…` (new branch on origin) |

**H2 is resolved.** The production release lineage no longer exists only on the VPS and laptop — it
is on GitHub for the first time, which was the stated purpose of this step.

## 3. CI run results

**Run [`34406342241`](https://github.com/romandesigns/stockspotter/actions/runs/34406342241)**,
`Validate`, event `push`, head `ba722698`. Triggered **automatically** by the `release/**` path added
in Checkpoint 2 — the first automatic CI validation of a release commit in this project's history.

**Job `Tests, lint and build` → SUCCESS.** Every step green:

`cargo test --workspace --locked` · client tests · client lint · client build · mobile `tsc` ·
**`pytest`** · `unittest discover`

Note `pytest` passed here — the one suite unavailable on this laptop, closing the Milestone B
environment gap.

**Job `Dependency advisories` → FAILURE**, one step only:

| Step | Result |
|---|---|
| Install cargo-audit | ✅ |
| **Rust advisories (RustSec)** | ✅ |
| `bun install --frozen-lockfile` | ✅ |
| **JavaScript advisories** | ❌ |

**This is the documented accepted exception, not green CI, and is not represented as such.** It is
`bun audit --audit-level=high` failing on the 9 known high-severity **transitive** advisories
(`minimatch` ×3, `nanoid` ×3, `image-size` ×2, `node-tar`) reached through
expo / react-native / vite / tailwindcss / eas-cli.

Not weakened. No `--ignore` added. No dependency upgraded. **No unrelated CI step failed**, so §3's
investigate-before-deployment condition was satisfied.

## 4. Exact deployed commit

```
ba722698af4fa2b387339eb017a2d58734f969c7
```

Confirmed identical in **both** `git rev-parse HEAD` and `ops/vps/.deployed-commit`.

**Deployment method:** the project's normal mechanism, nothing bypassed. The VPS checkout was
fast-forwarded (`git merge --ff-only FETCH_HEAD`) — required because `deploy.sh`'s `release/*` case
deliberately does *not* advance release branches; an operator promotes them on purpose. Then
`./ops/vps/deploy.sh` ran with every gate active:

- dirty-checkout protection ✅ (tree was clean)
- branch check ✅
- **remote-presence check ✅** — the Checkpoint 2 guard, which would have **refused** this deploy
  an hour earlier when the commit was not on origin
- token-length assertion ✅
- `--wait` health gates ✅
- post-deploy probes ✅ — *"Authenticated HTTP/WS, qualitative service, and web probes passed"*

Exit code **0**. No binaries were copied manually.

## 5. Production health after deployment

| Check | Result |
|---|---|
| Containers | all 5 healthy: `web`, `ws`, `qualify`, `auto-trader`, `discovery-review` |
| Alpaca feed | `alpaca ws: authenticated`; universe **12,635** symbols |
| Movers scan | `gainers=25 most_active=25` |
| Auto-Trader | `Alpaca PAPER account connection verified`, `connected and welcomed by ws-server` |
| Qualify service | healthy (deploy gate) |
| Public `/` | **200** |
| Public `/ws` | **101** Switching Protocols |
| Public `/api` without token | **401** — auth gate enforcing |
| Auth failures since deploy | **0** |
| Market event flow | unchanged — 302 distinct symbols observed in 60s |

## 6. Measurement collector startup verification

```
2026-09-09T22:08:43Z INFO ws_server::measurement: measurement capture enabled path=data/research
```

**Artifacts verified live**, not merely assumed. 53 episodes written and re-parsed on the VPS:

| Field | Present |
|---|---|
| episodes parsed | 53/53 |
| `openingContext` | **53/53** |
| `outcome` | **53/53** |
| `researchRank` | 49 |
| `momentumTrack` | 50 |
| ctx `preDetection` | 53 |
| ctx `ignition` / `halt` / `catalyst` / `funnel` / `market` / `momentum` | 46 / 31 / 27 / 27 / 27 / 23 |
| **horizons observed / censored** | **371 / 0** |
| `closeReason` | invalidated 52, inactivity 1 |
| `openedBy` | IgnitionDetector 27, FastFunnel 23, MomentumScorer 3 |
| `schemaVersion` | 1 |

Feature groups appear at plausible, *varying* rates — momentum on 23 of 53, catalyst on 27 — which is
what honest optional capture should look like. Uniform presence would have suggested fabrication.

### One diagnosis worth recording

The research file sat **empty for ~30 minutes** after deploy while signals were plainly flowing. §6
says to diagnose that immediately rather than wait, so I did.

Live measurement of the actual stream: **73 episode-opening** and **837 episode-closing** events per
minute. So episodes were definitely being created and closed — the pipeline was not stalled.

The cause was my own Milestone B design: a closed episode enters the pending set and is written only
after the **30-minute outcome window**, so its forward returns can accumulate. Collector started
22:08:43; first records appeared at ~22:42. **Working as designed, not a defect** — but it is a
property worth knowing operationally, because it means *no episode data exists for the first 30
minutes of any capture*, and an unclean process kill loses everything still pending.

## 7. Discovery recorder verification

Writing independently and actively:

- `DISCOVERY_AUDIT_DIR=/app/data/discovery-audit` (present in `.env`)
- 7 capture files, newest created **during** this verification (22:09), growing
- **No path collision** with `data/research/` — confirmed
- Dropped-record accounting independent: `discovery audit has gaps` count **0**

## §7 Session-start baseline counters — recorded 2026-09-09 22:44 UTC

| Counter | Value |
|---|---|
| Deployed commit | `ba722698…` |
| **measurement dropped** | **0** |
| **measurement write_errors** | **0** |
| **discovery lost_records** | **0** |
| episodes written (today, post-close) | 56 |
| `auto_trader_journal.jsonl` | 8,118 records |
| `data/` total | 15 GB |
| `data/discovery-audit` | 15 GB |
| **Disk available** | **270 GB** |

## Capacity assessment

Requirement was ≥15 GB free. **270 GB available** — 18× the floor.

| Need | Size |
|---|---|
| One raw session (discovery) | ~5.1 GB |
| Episode measurement | ~10–40 MB |
| Existing `data/` | 15 GB (already counted) |
| Export generation | ~0.2 GB compressed |
| **Headroom after a full session** | **~265 GB** |

Capacity is not a constraint. **Proceed.**

Worth flagging: `data/discovery-audit` already holds **15 GB** from earlier captures. Tomorrow's
session lands in date-stamped files, so the export can be scoped to the session cleanly — but that
directory will keep growing and eventually wants a retention decision. Not touching it now; that
would be the unrelated maintenance §4 rules out.

---

## Remaining: §§8–10

**Today's regular session ended at 16:00 EDT (20:00 UTC), before this milestone started.** The
capture is therefore tomorrow's session:

| Milestone | Time (UTC) |
|---|---|
| Capture already running since | 2026-09-09 22:08 |
| Market open | 2026-09-10 13:30 |
| Market close | 2026-09-10 20:00 |
| Export + verification | after 20:00 |

**The code is frozen at `ba722698` and will not be touched in between**, which is precisely what §12
requires. Capture is already live and accumulating, so tomorrow's session will be recorded end to end
without further intervention.

Deliverables still outstanding: total episode count, discovery record count, end-of-session integrity
counters, raw and compressed sizes, export manifest, checksum verification, local research path,
auto-trader records captured, and the suitability judgement. **None of these can be honestly reported
before the session exists.**

---

## Confirmation: no strategy parameter or trading behavior changed

The deployed commit is the approved Milestone B checkpoint, whose entire footprint on existing
strategy code is 16 lines of module re-exports plus catalyst timestamp plumbing (pure additions, zero
deletions). Unchanged and unread by any new code:

Fast Funnel thresholds · Ignition thresholds · momentum 0.60 gate · Auto-Trader momentum gate · 0.40
deterioration exit · consolidation thresholds · halt thresholds · catalyst qualification · strategy
enablement · position sizing · stops · targets · timeouts · max concurrent positions · production
event ordering and filtering.

Nothing was tuned. Auto-Trader is running its frozen baseline. The only production changes made were:
advancing the checkout to the approved commit, and running the standard deploy script.

## `git log --oneline --decorate -5`

```
ba72269 (HEAD -> release/operating-run-20260907, origin/release/operating-run-20260907,
         origin/gpt/audit-remediation-20260909, gpt/audit-remediation-20260909)
        Build causal alpha measurement pipeline
0544790 Gate release deployments on remote CI lineage
725179b Harden authentication and realtime stream resilience
bafae88 Sign in once per device instead of re-entering the key every launch
54313cc Operate discovery coverage recording with reconciled Alpaca paper trades
```

## Access note

A deployment key (`claude-code-milestone-c-20260909`) is currently authorized on the VPS. It should
be revoked once §§8–10 are complete:

```sh
ssh wavystack@100.88.87.41 "sed -i '/milestone-c/d' ~/.ssh/authorized_keys"
```

The older orphaned `stockspotter-diag` key is also still present and holds no private half anywhere;
worth removing in the same pass.
