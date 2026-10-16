# Windows desktop delivery

The desktop embeds the client interface. A website deployment does not replace
that interface. `Master platform builds` produces unsigned Windows CI installers
for each master push; producing an installer does not install it on a device.

The desktop rail's **Desktop version and updates** control identifies the native
version, embedded frontend commit and source tree. These do not identify the
remote backend, Android build or an unmerged pull request. Git-unavailable builds
show unknown; local source edits show dirty.

## Signed release

1. Merge the reviewed change using normal required checks. Keep the Tauri config
   and standalone desktop Cargo package versions equal. Version 0.9.4 refreshes
   the embedded client from reviewed master (Today's Shortlist, journal, Bullish
   Momentum cards) and supersedes 0.9.3 while preserving
   the unrelated 0.9.1 draft. Do not publish that older draft after a newer
   release or replace existing release assets.
2. Dispatch `desktop-release.yml` on master with `publish=true` and `source_sha`
   equal to the exact reviewed current master commit. The workflow refuses source
   drift, dirty input, existing tags and existing releases, including drafts.
   Server/client validation and desktop RustSec must pass before signing.
3. The workflow creates a draft. Before publication, verify the run's source,
   installer hashes, every updater signature against the configured public key,
   and the updater JSON's version, URLs and signature-to-artifact correspondence.
   Keep the trust key compatible with existing clients. Updater signatures are
   distinct from Windows Authenticode signatures.
   The canonical installation is current-user NSIS. The generic Windows updater
   entry must reference that signed setup.exe, matching the installed format;
   the format-specific MSI and NSIS entries remain available.
4. Back up the installed executable, shortcut and app data with the app stopped.
   Install the reviewed release, verify the running executable's hash and path,
   then confirm native version and embedded commit in the desktop control.
5. Publish only the verified draft when release is authorized. Recheck the public
   latest.json endpoint after publication. Keep old artifacts and backups until
   rollback and the installed build are verified.

The app checks at startup after access is granted and allows manual retry. A
failed check is not a claim that the interface is current. Availability does not
install anything: **Install update and close app** is an explicit user action.
Tauri verifies the updater signature during download. Windows hands off to the
installer and closes the app; reopening may be needed.

Installing 0.9.2 manually proves neither download nor updater installation.
Checking against its own feed exercises only the no-newer-release branch. The
download/install path needs a distinct, newer reviewed release to test end to end.

## Research panel preference

The rail's **Research panels** control enables IAC and Extreme Momentum on the
embedded desktop client and web client. Its saved choice overrides the build
recipe, including an explicit off choice. Without a saved choice, the reviewed
build default applies; desktop remains off by default. A storage error applies
the choice for this launch and reports that it could not be saved.

A disabled server, rejected credentials or unsupported protocol stops the
research connection and displays the refusal. Switch off and on to retry after
resolving it. Capacity and transport failures retain bounded backoff. Turning
research off closes only that separate socket; ordinary market data stays live.
These descriptive panels do not change alerts, rankings or trading.
