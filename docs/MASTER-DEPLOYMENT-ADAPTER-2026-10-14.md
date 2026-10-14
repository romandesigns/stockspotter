# Master deployment adapter (schema 2) — design, challenges, and what is NOT done

`ops/vps/deployment_host.py` is the real Linux host adapter for
`ops/vps/deployment_transaction.py`. The transaction module decides; this
adapter performs. Nothing here has been executed against the VPS: this
document and the code landed from a development worktree with no SSH, no
network and no production mutation. Every claim below is either a property of
the code in this repository or an explicitly labelled observation reported by
the operator.

## Observed baseline (operator-reported, 2026-10-14)

| | |
|---|---|
| Production root | `/opt/apps/stockspotter` |
| Root checkout | `2472e0f94186d59409e8dcbf0b0bdf168362bcd0` on `release/mover-provenance-20261011` |
| Actually running `web` + `ws` | built from `c28fb194849a4fdb532f2dbe6b3938b0c61c7b54` |
| How that runtime was launched | overlay compose files under `/home/wavystack/stockspotter-iac-coverage-20261013/release-evidence/`: `active-compose.json` (ws and the rest) and a **separate** `active-web-compose.json` (web) |
| Running image IDs | `web dd1bcc6e…c941`, `ws ad4b33bb…2dcd`, `qualify 5a7a9f84…c3f0`, `auto-trader 5d96af96…d080`, `discovery-review 90d5563b…f077` |
| Compiled `ws` artifact | `/app/ws-server` = `7c849fa09914680dadcb6453e0a3e342f2bacc3deb0db16ebc061c79d4da53a0` |
| Public web/server feature flags | already enabled in the live build |
| Disk | ~16 GB free of 387 GB |
| Timer | `stockspotter-deploy.timer`, every 2 min, `ExecStart=ops/vps/deploy.sh` |

This is a **mixed baseline**: the compiled runtime and the checkout are
different commits, so a single "deployed commit" field cannot describe the
host. That is why schema 2 carries `source` (compiled runtime) and
`checkout_source` (root Git) separately, and it is why this adapter exists
instead of a patch to `deploy.sh`.

## Codex host review, 2026-10-14 — what was accepted and what was contested

Codex ran 58 adapter tests successfully and still rejected the draft for
production. Six of its nine blockers were correct and are now fixed in code;
one was already fixed before the review and is contested here; two were about
the frontend flag and are addressed as described.

| # | Codex blocker | Outcome |
|---|---|---|
| 1 | Adoption must never rewrite `ops/vps/.deployed-commit`; use a private controller marker; drop `--adopt-marker` | **Accepted, fixed.** Verified against `deploy.sh` directly. |
| 2 | `env_hash` must bind the full per-role effective environment, not a flag subset | **Accepted, fixed** — with one correction of my own to Codex's framing (below). |
| 3 | A stricter candidate rule must not make adopting the real baseline impossible | **Accepted, fixed** (recorded baseline-only exemptions). |
| 4 | Source equivalence needs real provenance and per-role decision receipts; don't silently unpin | **Accepted, fixed**; partial-role promotion explicitly not built. |
| 5 | Install must be atomic/fail-closed; the timer build-only; real Docker rollback proof; no fabricated evidence | **Accepted, fixed** (`rehearse` gate, optional-file manifest coverage). |
| 6 | Preserve the compiled frontend flag via recipe args and the source archive; no automatic true default | **Accepted**, already true; the live flag is enabled, so the adopted value must be `true`. |
| 7 | `health()` unconditionally refuses outside 20:00–04:00, so adoption cannot run now and rollback always ends Held | **Contested — outdated.** Already fixed before the review. |
| 8 | `up()` passes only the base compose file, so IAC/Extreme overlay flags are lost on promotion *and* rollback; no shared mutable tags | **Accepted, fixed** — the largest change in this revision. |
| 9 | `apps/client/Dockerfile` has no `ARG VITE_EXPERIMENTAL_STRATEGIES`, so a build arg does not compile `true` | **Accepted**, already fixed; bundle parity is still reported as honestly unknown. |

### Blocker 1, verified rather than assumed

`deploy.sh` reads `LAST_DEPLOYED="$(cat ops/vps/.deployed-commit)"` and exits 0
only when `AFTER = $LAST_DEPLOYED`. Today the checkout and that marker both say
`2472e0f9…`, so the two-minute timer no-ops. Writing the *running* build
`c28fb194…` there would make `AFTER != LAST_DEPLOYED` true, and the next timer
tick (≤ 2 minutes) would run `docker compose -f ops/vps/docker-compose.yml
build` and `up -d` — rebuilding the checkout's commit from the **base** compose
file and destroying the live overlay configuration. Codex was right, and the
consequence is worse than "the marker is wrong".

So: `LEGACY_MARKER_PATH` is read-only everywhere in the module, `marker()` and
`write_marker()` use `<control dir>/deployed-commit` (0600 inside a 0700
directory), `--adopt-marker` no longer exists, the adoption receipt records the
legacy value read-only, and `inspect` prints an explicit `legacy_timer_risk`
warning whenever the legacy marker and the checkout disagree. A test asserts
that a full promotion and a full rollback both leave the legacy file byte-identical.

Because the legacy timer would *also* rebuild right after a successful
promotion moved the checkout to `master`, `promote` and `rehearse` now refuse
unless `systemctl is-enabled`/`is-active` report `stockspotter-deploy.timer` as
disabled-and-stopped. `systemctl` being unusable counts as armed, not as absent.

### Blocker 8, and why the fix is a private recipe rather than extra `-f` flags

The live stack was created by overlay compose files that are not in this
repository and set at least `IAC_ENABLED` and `EXTREME_MOMENTUM_ENABLED`, and
`web` was launched from a *different* overlay file than the rest. Passing those
untracked files to `up` would make the controller depend on paths in a
developer's worktree, and would still not be the resolved configuration.

`adopt` therefore captures the **observed effective configuration** of all five
containers into a private immutable recipe: exact image IDs, per-role
environment, bind mounts *with their real sources* (the live data mount is the
IAC worktree's `data/`, not the production checkout's), loopback port
publication, logging driver and options, restart policy, command, entrypoint,
user and working directory. `up()` writes that recipe as a standalone compose
JSON document and runs `docker compose -p stockspotter-vps -f <recipe>`:

- **Promotion** replays the recipe with only the five image IDs substituted, so
  a candidate inherits the live configuration exactly.
- **Rollback** replays it verbatim with the baseline image IDs.
- The base compose file is never passed to `up`, so no commit being promoted
  can change how the stack is launched, and the recipe has no `build:` section.
- **No shared mutable tag is ever written.** The previous draft tagged pinned
  images onto `stockspotter-vps-<role>:latest` — exactly what `deploy.sh` and
  `deploy-chart-web.sh` read. Rollback images are now retained only under
  `stockspotter-rollback-<role>:<source>`, and a test asserts no `:latest` tag
  is ever written.
- `capture_recipe` refuses what it cannot faithfully reproduce: a container in
  another compose project, a non-default network, a non-absolute bind source.

Fidelity is proved, not assumed. `conformance_failures` compares the observed
containers against the document that asked for them — image, every declared
variable's value, `.env` coverage, bind sources and destinations, port
publication, restart policy, logging, command, entrypoint, user, working
directory — and additionally refuses any variable *beyond* what the document
declares. It runs after every `up` **and on every dwell iteration**, so a lost
overlay variable or a dropped log rotation is a `Rejected` proof, which the
transaction turns into a rollback.

One asymmetry is deliberate: the base-compose preflight runs only for a
*candidate* recreation, never when restoring the adopted baseline. A preflight
able to refuse a rollback would defeat the thing this controller exists to
guarantee. A test covers exactly that case.

### Blocker 2, accepted with a correction

`env_hash` now binds, per role, every effective application environment
variable by name with its value's SHA-256 — `.env`-supplied and
overlay-supplied, secret or not — plus the frozen-file digests. Only two
exclusions remain, both recorded as drift in the receipts: build identity
(`STOCKSPOTTER_COMMIT`, different in every candidate by design) and
image-provided variables on an explicit **exact-name** allowlist
(`IMAGE_ENV_ALLOWLIST`: `PATH`, `PYTHON_VERSION`, …). An image-provided
variable that is *not* on that allowlist is bound, so it fails the promotion
proof rather than being forgiven.

The correction: Codex asked for the hash to bind the *observed* containers.
Taken literally that breaks rollback. Schema 2 uses `env_digest()` as the
**pre-restore guard** (`restore()` holds if it disagrees with the baseline). If
the hash tracked the live containers, then a candidate whose environment
drifted would fail its proof, the restore guard would then disagree, and the
transaction would end `Held` **with the bad candidate still running** — the
opposite of what blocker 8 demands. So the split is:

- **operator-controlled configuration → `env_hash`**: `.env`, the frozen files
  and the recipe's declared per-role environment. Drift here is `Held`, which
  is correct: a human changed the configuration mid-flight.
- **what is actually running → the conformance proof**: equality against the
  recipe on every dwell iteration. Drift here is `Rejected`, which rolls back.

Both are strict; neither can be satisfied by the other. A credential is never
written to a controller file: a variable that would have to be reproduced
literally *and* looks like a secret (or whose value digest matches a
secret-named `.env` value) is refused with an actionable message, and secrets
reach containers only through compose's `env_file` pointing at the host's own
`.env`.

### Blocker 7, contested

Codex read `health()` as refusing outside the window. It does not, and did not
at the time of the review. `allowed()` is consulted by the transaction for
candidate promotion only; `health()` consults the clock solely inside
`if commit == self.candidate_source`. Adoption, reconciliation and rollback
proof are never clock-gated. `test_the_hard_stop_never_blocks_a_baseline_proof`
adopts, reconciles and proves health at 11:00 local with a deadline already
past, and `test_crossing_the_hard_stop_during_the_dwell_rolls_back` shows the
candidate-side stop still forcing the rollback it exists to force.

## Challenges to the obvious strategy

**1. "Just extend `deploy.sh`."** Refused. `deploy.sh` merges `origin/master`
into the production checkout *before* the CI gate, has no journal, and has no
rollback: if `up -d --wait` or the health probes fail, the checkout and the
containers are already replaced and it exits non-zero with the host in the new
state. It is also what the 2-minute timer runs. Extending it would put a
rollback-capable transaction behind an unattended trigger. The adapter is a
separate controller; `deploy.sh`, `deploy_guard.py`, `deploy-chart-web.sh` and
both existing systemd units are untouched.

**2. "Hash the whole container config into `env_hash`."** Still refused in that
literal form, for the reason above *and* because `ws`'s Dockerfile sets
`ENV STOCKSPOTTER_COMMIT=<arg>` (so the `ws` container's environment must
differ between baseline and candidate) and the compose file's log-rotation
block activates only on recreation. What replaced the flag subset is described
under blocker 2: the full operator-controlled environment by digest, plus a
strict conformance equality on the running containers.

**3. "Verify the frontend's compiled commit."** Still not possible, and the
honest answer is recorded rather than worked around. `.dockerignore` excludes
`.git`, so `apps/client`'s `vite.config.ts` build-identity probe resolves to
`unknown` in *every* Docker-built web image, including the live one.
`provenance()` therefore states
`frontend_build_identity: unknown-by-construction`, and web identity is proved
by three things that are real: the immutable image ID, the
`org.opencontainers.image.revision` label, and a byte comparison of the served
`http://127.0.0.1:3000/` body against `/app/dist/index.html` inside the running
container (plus that bundle's digest in the provenance receipt). A
build-source `ARG` that would make the compiled identity real is a reasonable
future change; it is not needed for this fix and is not attempted here. `ws`
identity is the real compiled stamp from `/research/completeness`
(`report.commit`), corroborated by the `sha256sum` of `/app/ws-server` read
inside the container.

**4. "A prepared candidate stays valid."** It does not. `flock` is released
when the controller exits and is lost entirely across a reboot, so a candidate
prepared before a reboot could be promoted against a host whose lock history
is gone. `prepare` records `/proc/sys/kernel/random/boot_id` and a timestamp in
its own private `prepared.json`; `promote` refuses a different boot id or an
expired candidate. This lives in the adapter, not in the schema: boot identity
is a host fact, and the schema-2 state and its 21 tests are left intact.

**5. "The Android gate is part of the server gate."** It is not.
`deploy_guard.py` requires the two server jobs to succeed and accepts
`success`, `failure` or `skipped` for the mobile job; a skipped mobile job
means the change set did not touch that surface. The adapter does not require
an Android pass for a server-only candidate, and records the server gate's own
evidence line plus an explicit `android_gate` field
(`not-evaluated-by-this-controller`) in the receipt so a skipped Android run is
never read as an Android pass.

**6. "A read-only diff of the protected roles is provenance."** It is not. That
`2472e0f9… → master` changes only 62 lines of Python and tests for the three
protected roles says what *would* change if they were rebuilt; it says nothing
about what the running images actually are. `provenance()` reads the real
thing: image ID, image creation time, repository digests, revision label, the
`sha256sum` of each role's baked artifact read inside the running container,
and the compose files and project working directory each container was created
from. `adopt` requires an explicit `--role-decision <role>=rebuild|hold` for
**all five** roles, binds those decisions to a digest of that provenance, and
records both in the policy and the receipt. If the provenance later differs,
`prepare` is `Held` until re-adoption. `--expect-artifact <role>=<sha256>` lets
the operator assert a digest (for example the reported
`7c849fa0…53a0` for `ws`) and have it verified at adoption; an unverifiable
assertion is `Held`, not accepted.

## Commands

All six are explicit; none of them is implied by another.

| Command | Lock | Mutates | Purpose |
|---|---|---|---|
| `inspect` | no | nothing | Observe the host and write a private observation receipt. Does not fetch; reports the already-known `origin/master` ref, both markers, the legacy timer state, the per-role effective environment by name, and full provenance. |
| `adopt` | yes | control dir only | Record the observed baseline as the committed schema-2 receipt, capture the private full-fidelity recipe, and pin the freeze, flags, per-role decisions, baseline exemptions and artifact digests, after a full runtime proof. Provenance is marked `observed-not-deployed`. Never touches `ops/vps/.deployed-commit`. |
| `prepare` | build only | candidate images, staging | Fetch the tip, run the preserved CI gate, build all five roles from a tracked-only `git archive` of the exact `master` commit, retain private rollback tags, write `prepared.json` (boot id bound). |
| `rehearse` | yes | containers, checkout | Promote a real candidate with its dwell proof failed **on purpose**, so the transaction performs a real rollback, and record that rollback as evidence. |
| `promote` | yes | containers, checkout, private marker | The schema-2 promotion: legacy timer disarmed, recorded rollback rehearsal for this baseline, window, zero exposure, recreation from the recipe, multi-iteration dwell proof, commit or automatic rollback. |
| `reconcile` | yes | per journal | Finish or roll back an interrupted transaction. |

Exit codes: `0` success, `2` deferred (retry later), `3` rejected (the
candidate or the runtime failed a release check), `4` held (explicit human
recovery required), `1` usage/other.

## Safety properties implemented in the adapter

- **Legacy isolation.** `ops/vps/.deployed-commit` is read-only; the private
  marker is `<control dir>/deployed-commit` at 0600 in a 0700 directory; no
  shared image tag is written; `promote`/`rehearse` refuse while the legacy
  timer is armed.
- **Configuration preservation.** Promotion and rollback both replay the
  private recipe; conformance to it is re-proved on every dwell iteration;
  undeclared environment is refused.
- **Window and hard stop.** `allowed()` is true only between 20:00 and 04:00
  America/New_York and only with at least 45 minutes left. The hard stop is
  re-checked on every dwell iteration, but only *while the candidate is the
  live build*.
- **Failure classification.** A failed runtime proof is `Rejected`, so the
  transaction restores. An unusable host command is `Deferred`. A candidate
  whose own compose file does not resolve is `Rejected`, not `Deferred`.
- **Zero exposure.** `exposure_zero()` runs the repository's own
  `ops/release_preflight.py` (read-only Alpaca `/v2/positions` and
  `/v2/orders?status=open`) inside the existing `qualify` container, and
  additionally requires the auto-trader's `openPositions` to be empty. Any
  inability to *prove* zero is false, not true.
- **Rollback is proved before it is relied on.** `promote` refuses without a
  `rehearsal.json` whose restored state equals the current committed baseline
  and whose policy digest matches; a successful promotion deletes it, so each
  promotion needs its own rehearsal against its own baseline.
- **Baked artifacts.** Each role's single baked artifact is digested inside the
  running container and pinned at adoption; a restoration must reproduce those
  bytes, so a rollback cannot be satisfied by a tag that points elsewhere. A
  role whose image has no `sha256sum` records `null` and is honestly
  unenforceable.
- **Baseline exemptions.** `--baseline-exempt <role>=unbounded-logs|restart-policy`
  waives exactly those two invariants, for exactly the adopted baseline image
  of that role. Bind shadowing, loopback-only publication, flag parity and the
  order-flow kill switch are never waivable.
- **Immutable images.** Candidate image IDs are resolved with
  `docker image inspect`, written into the recipe and re-verified after `up`.
- **Bounded builder / headroom / freeze / bind shadowing / secrets.** As
  before: the existing `stockspotter-ui-guarded-20261012` builder is verified
  and never created; a 10 GiB free-space guard defers; frozen blob ids are
  compared before the checkout moves; only `/app/data` may be bind-mounted, for
  three roles; container environments are hashed, never printed, and no command
  line carries a credential.
- **Probes do not depend on a compose file.** All in-container probes use
  `docker exec` directly, so `inspect` works before any recipe exists and no
  probe can be influenced by the compose file in the checkout being deployed.

## Isolated staging rehearsal

`ops/vps/staging_rehearsal.py` and `ops/vps/staging_broker.py` answer the one
question that matters before production is touched: **is the captured recipe
actually faithful?** `plan`/`write` derive an isolated staging stack from the
recipe and `compare` checks a saved `docker inspect` against it, using
`deployment_host.conformance_failures` — the production proof's own
implementation, so a staging pass cannot mean something production would refuse.

Isolation is enforced in code and re-checked by `verify_isolation`: a distinct
compose project (`stockspotter-staging`, so no production container can be
recreated or removed), **no published ports at all**, an `internal: true`
network (no route to the real broker or feed), **no `env_file`** with every
`.env` variable replaced by a synthetic marked value, bind sources rewritten
into a throwaway staging directory, and no `build:` section. Seven faults are
injectable (`crash-loop`, `probe-failure`, `missing-bind`,
`drop-overlay-env`, `unbounded-logs`, `broker-open-position`,
`broker-unreachable`), and a fault may not break isolation.

**Staging exposure coverage.** The original HTTP fixture exercises refusal only.
The additional `staging_tls.py` fixture preserves the exact HTTPS paper hostname,
uses an ephemeral certificate and synthetic DNS inside the isolated namespace,
and supplies the trust file only to staged containers. Four real HTTPS tests
exercise the byte-unchanged preflight: synthetic zero exposure, open positions
or orders, blocked or unavailable broker, and untrusted-certificate refusal.
This is synthetic proof, never proof of the live account. A complete Docker
Transaction/Host promotion and rollback rehearsal is still outstanding.

## Tests

```sh
cd ops/vps && python -m unittest test_deployment_host test_deployment_transaction \
    test_deploy_guard test_staging_rehearsal test_staging_tls
```

239 tests passed locally on 2026-10-14: 155 adapter, 30 staging-document,
21 transaction, 29 deploy-guard and four verified-HTTPS fixture tests.

`test_deployment_host.py` drives a fake command runner that answers the
adapter's *real* argument vectors and mutates its own world the way the host
would. The fidelity property that makes the new tests mean something: the fake
world's containers are **derived from whichever compose document the adapter
actually passes to `-f`**, and the baseline is launched the way the real host
launched it — from overlay files that set `IAC_ENABLED`/
`EXTREME_MOMENTUM_ENABLED` and bind data from the IAC worktree, neither of
which `ops/vps/docker-compose.yml` mentions. A controller that recreated from
the base compose file would therefore really lose them, and the fake asserts
outright that `up` is never handed the base compose file. `docker compose
config` is answered per checked-out commit, so a rollback's preflight differs
from a promotion's. It is still a model: it proves what the adapter *commands*
and *refuses*, never that a deployment happened.

## Controller installation (fail-closed, no legacy fallback)

`ops/vps/deployment-controller.sh` is a wrapper, not an entry point: it
requires an immutable controller installation (default
`/opt/stockspotter-controller`) containing `deployment_host.py`,
`deployment_transaction.py`, `deploy_guard.py` and `release_preflight.py`, and
refuses to run if any file is missing or if the installed digests do not match
the digests recorded at installation time. The staging files are optional, but
if present they must also be covered by the manifest — an uncovered file in the
installation directory is a file nobody verifies. It never falls back to the
copies in the candidate checkout and never calls `deploy.sh`.

`ops/vps/stockspotter-prepare.service` / `.timer` run **`prepare` only** —
build and gate, no promotion — and ship disabled. Promotion stays manual, and
refuses outright until a rollback rehearsal exists. The existing
`stockspotter-deploy.timer` is not modified, not enabled and not disabled by
this change; disabling it is a deliberate operator act and a precondition of
any promotion.

## Frontend experimental flag

`apps/client/Dockerfile` declares `ARG VITE_EXPERIMENTAL_STRATEGIES=false` and
exports it for the Vite build, so the flag is a real, defaulted build input
instead of an unreachable one. Default `false` keeps every existing build path
(`deploy.sh`, `deploy-chart-web.sh`, CI) byte-identical, because
`ops/vps/docker-compose.yml` passes no build args. The adapter builds `web`
with the adopted value, stamps the
`io.stockspotter.experimental-strategies` label on the image and refuses a candidate whose label disagrees with the
adopted policy.

The operator reports the live bundle has these flags **enabled**, so the
adopted value must be `--experimental-strategies true` — otherwise the
candidate would compile them off and silently regress the live UI. The live
image carries no such label, so the adopted value remains an **operator
assertion** recorded at adoption, corroborated only by the served-bundle byte
digest; it is not a measurement of the compiled bundle. That is stated as an
unknown rather than papered over.

PR #65 lands the same `ARG`. This change keeps its version minimal on purpose;
the duplicate resolves forward at merge.

## Not completed (explicitly)

- **No deployment, no rehearsal.** Nothing in this branch has run on the VPS.
  There is no inspect output, no adoption receipt, no build, no promotion, no
  staging run and no rollback evidence from the real host. The `rehearse`
  command exists; it has never been run against Docker.
- **Partial-role promotion is not built.** Per-role decisions are recorded and
  enforced, but a `hold` decision *blocks* promotion rather than promoting the
  other four roles. Promoting a subset would need per-role image sets and
  per-role rollback in the schema-2 core, which is a separate change; refusing
  is the fail-closed behaviour in the meantime.
- **The rehearsal gate is a gate, not a proof of authorship.** `rehearsal.json`
  lives in the controller's own 0700 control directory and is checked against
  the current committed baseline and the current policy digest, so a fabricated
  record would have to match the live host's baseline state exactly — which
  means the host really is in the restored state. But the controller does not
  sign it, and a user with write access to the control directory could still
  forge one. It stops an *absent* rehearsal and an accidentally stale one; it
  does not stop a deliberate operator. Signing it (or deriving it from the
  journal's own transaction id in a tamper-evident way) is a possible later
  hardening and is not implemented.
- **Re-adoption is required between promotions.** A successful promotion
  changes the baseline images and therefore the provenance the per-role
  decisions were bound to, so the next `prepare` is `Held` until `adopt` runs
  again against the new baseline. This is deliberate (every promotion gets a
  fresh explicit decision and a fresh rollback rehearsal), not an oversight,
  but it does mean promotions are not unattended even in principle.
- **Mixed-baseline ancestry is unresolved.** `ancestor()` answers truthfully
  with `git merge-base --is-ancestor`. If the running
  `c28fb194849a4fdb532f2dbe6b3938b0c61c7b54` is not an ancestor of the `master`
  tip, `prepare` is *rejected* and no override exists in this code. Confirming
  that ancestry is a real, pending step.
- **The frontend's compiled identity is unknown, not proved.** See challenge 3.
  A reviewed build-source `ARG` could fix it later; nothing here depends on it.
- **Complete staged promotion and rollback remain unverified.** The HTTPS
  preflight fixture passes, but it does not establish full Docker cutover proof.
- **Builder provisioning is not automated.** If
  `stockspotter-ui-guarded-20261012` is absent or unbounded, `prepare` defers
  and a human provisions it.
- **No automated receipt for Android.** The adapter records that the Android
  gate was not evaluated by it; it does not query the mobile workflow.
- **Shell syntax passed `bash -n` on the Linux VPS.** This validates parsing,
  not installation or runtime behavior.
- **Automatic production promotion is unfinished.** The two-minute timer invokes
  build-only `watch`. It reuses a matching prepared candidate, refreshes after
  expiry/reboot, and uses a separate build lock. It does not promote or waive
  the window, CI, exposure or rollback requirements. Do not describe this as a
  completed master-to-production transition.
- **`.env` parsing is an approximation of compose's.** `Files.env_pairs` strips
  one matching pair of surrounding quotes, as compose v2 does. A variable this
  parser reads differently from compose will fail to match the observed
  container value and be reported (`Held`) rather than silently accepted — fail
  closed, but it is a real difference that only the host can settle.

## Required real rehearsals before any promotion

1. `inspect` on the host; confirm the five image IDs, the compiled `ws` commit,
   the baked artifact digests, both markers, the legacy timer state, the
   per-role effective environment, the overlay `config_files` of each container
   and the frozen digests against this document.
2. Confirm `master` contains the live `web`/`ws` lineage (`git merge-base
   --is-ancestor c28fb19… master`).
3. `adopt --confirm --experimental-strategies true --role-decision <role>=…`
   for all five roles, plus any `--baseline-exempt` the real logging/restart
   configuration needs, plus
   `--expect-artifact ws=7c849fa0…53a0`. Keep the receipt and the captured
   recipe, and **diff the recipe against the two overlay compose files by hand**
   — that is the one step no test here can perform.
4. Staging fidelity: `staging_rehearsal.py write`, run the printed plan, then
   `compare`. Expect zero fidelity failures, then repeat with `--fault` to
   confirm the faults are detected.
5. `sudo systemctl disable --now stockspotter-deploy.timer`, and confirm
   `inspect` reports it disabled/inactive.
6. `prepare` inside the window on a day with no exposure; check the build
   receipt, the five candidate image IDs, the revision labels and the retained
   private rollback tags.
7. `rehearse --confirm`: a real recreation onto the candidate followed by a
   real rollback. Keep the evidence and verify by hand that the restored
   containers carry the overlay flags, the IAC data bind and the `ws` artifact
   digest `7c849fa0…53a0`.
8. Only then `promote --confirm`.
9. Only after that, consider enabling `stockspotter-prepare.timer`
   (build-only).

## Pending review of additional Codex corrections

The CPU quota, active-account/authentication and exact integer zero checks from
Claude's interrupted final implementation are retained. Negative tests cover
unbounded or mismatched CPU limits, inactive account, absent authentication,
and boolean/string exposure counts. The watcher now polls every two minutes,
serializes builds and does not rebuild an unchanged, valid candidate. Regression
tests cover reuse, expiration, reboot, baseline drift and concurrent preparation.
These changes and the TLS addition require renewed Claude review. No controller
has been installed, legacy timer disarmed or production checkout changed.

The initial 234-test suite also passed in a private Linux staging copy. An
additional real-Docker fixture used the existing immutable Python image on
internal networks with synthetic credentials and verified TLS, no published
ports and no production data mounts. Zero exposure, positions, orders, blocked
account and broker failure behaved as expected; before/after IDs, images,
start times and configuration hashes of all five production services matched.
Each owned fixture container/network was removed after its case. This does not
replace the still-pending complete controller promotion/rollback rehearsal.
The stage Host now requires an explicit disposable owner root and resolves
all bind sources inside it; production paths, traversal and named volumes are
refused. These additional changes remain pending Claude review.
