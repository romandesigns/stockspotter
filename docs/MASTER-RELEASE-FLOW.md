# Master-Based Release Flow

`master` is the canonical integration branch. Keep feature work on short-lived
branches created from `master`, open a pull request, and merge only after the
required `Tests, lint and build` and `Dependency advisories` checks pass. Do
not develop independently on a long-lived production branch.

Each artifact must identify the exact merged `master` commit:

- **Web and server:** the VPS deploy timer follows `master`; `deploy_guard.py`
  verifies the required server checks for the exact commit before the full
  Compose deployment.
- **Desktop:** a master push affecting the desktop client runs server/desktop
  validation, then builds and publishes the Windows Tauri release.
- **Mobile:** after the full Validate workflow succeeds, Android and iOS EAS
  production builds run for the same `master` commit when mobile or shared
  client inputs changed. The mobile advisory gate remains mandatory. EAS builds
  do not submit to app stores.

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

The `Mobile EAS production build` workflow requires an `EXPO_TOKEN` Actions
secret. It is not present in the repository secrets checked on 2026-10-08. Add
it through GitHub's secret manager; never commit or print the token. Desktop
publishing continues to require the existing Tauri signing secrets.
