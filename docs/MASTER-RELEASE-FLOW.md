# Master-Based Release Flow

`master` is the canonical integration branch. Keep feature work on short-lived
branches created from `master`, open a pull request, and merge only after the
required `Tests, lint and build` and `Dependency advisories` checks pass. Do
not develop independently on a long-lived production branch.

Merging to `master` validates; it does not publish, build for production or
deploy. Each artifact is produced by a separate, explicit step and must
identify the exact `master` commit it was built from:

- **Web and server:** as of 2026-10-08 the VPS checkout is on
  `release/begun-window-20261004-r1`, not `master`, so a merge to `master`
  does not deploy. `ops/vps/deploy.sh` fast-forwards automatically only when
  the checkout is on `master`; on a release branch it refuses unless the
  checkout already equals the branch tip. `deploy_guard.py` verifies the
  required server checks for the exact commit before the Compose deployment.
- **Desktop:** a master push or pull request affecting the desktop client runs
  server/desktop validation only. A release is built by dispatching
  `Desktop Release` by hand from `master` with `publish` ticked. That run
  refuses a version whose tag or release already exists, so bump
  `apps/client/src-tauri/tauri.conf.json` first, and it creates a draft; the
  draft is not served to the updater endpoint until it is published by hand.
- **Mobile:** Android and iOS EAS production builds are queued by dispatching
  `Mobile EAS production build` by hand from `master`. The run first executes
  the full Validate workflow for that commit, with the mobile advisory gate
  always on. EAS builds do not submit to app stores.

The "master only" condition in both workflows guards against accidents, not
against a deliberate dispatch of another branch's copy of the workflow file:
GitHub runs the dispatched branch's copy. No GitHub environment protects these
jobs today.

The mobile advisory check runs when mobile code, shared types, JavaScript
dependency manifests, or the advisory-gate implementation changes. A skipped
mobile check means the change set did not affect that surface; it is not an
advisory waiver. The repository ruleset requires this check alongside the
two server checks so mobile changes cannot merge while the mobile gate fails.

The existing `release/begun-window-20261004-r1` branch is a transition/rollback
reference. Retire it only after the production checkout has been deliberately
migrated to `master`, the exact deployed commit and healthy services have been
verified, and rollback instructions have been recorded. Do not switch the
production checkout while the timer is active unless the deployment window and
full-stack restart are intended.

## Required automation configuration

The `Mobile EAS production build` workflow requires the `EXPO_TOKEN` Actions
secret, present in the repository secrets since 2026-10-08; never commit or
print the token. Desktop publishing requires the existing Tauri signing
secrets.
