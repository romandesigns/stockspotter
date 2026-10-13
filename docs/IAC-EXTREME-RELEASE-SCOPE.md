# Disabled web/server release scope

The experimental protocol decoder and lifecycle types live in
`apps/client/src/lib/experimentalStrategies.ts` and, for the native app, in a
hand-mirrored copy at `apps/mobile/src/lib/experimentalStrategies.ts` guarded by
`apps/client/src/lib/experimentalParity.test.ts`. The shared-types package, root
JavaScript manifests and root lockfile are identical to the reviewed base commit
`5807d6116f5c793d6fd62ac255f443620ce21fa8` — the experimental contract is still
deliberately absent from the shared protocol package.

The ordinary shared protocol is unchanged; only an independently opted-in
web/desktop/mobile research socket can request the new messages, and only via an
`experimentalOnly` hello on its own connection. Backend flags and both frontend
feature flags default off. This release does not activate strategies, capture a
cohort or change orders or notifications.

## Native scope (added after the base commit)

Native delivery is implemented and off by default behind
`EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES=true`. The mobile application is therefore no
longer identical to the base commit; the changed surface is the new experimental
module/hook/panels, the `Section` primitive extraction, the Radar-tab mount point,
the `tools/native-experimental/` lifecycle regression harness with its Validate
step, and a mobile opt-in in `experimental_delivery_allowed`
(`crates/ws-server/src/server.rs`).
Ordinary mobile sockets are unchanged. Native distribution is still blocked, for
the same reason as before: see the advisories paragraph below.

`docs/IAC-EXTREME-NATIVE.md` records this transport addendum in full and names the
one sentence of the measurement contract it supersedes
("Native mobile delivery is deferred."). `docs/IAC-EXTREME-MOMENTUM-V1.md` is a
hashed historical measurement contract and is byte-identical to `cc03d364`; no
native work edits it.

This is a scope reduction, not dependency remediation. Mobile remains blocked by
the braces and node-forge advisories. No advisory waiver, alias, dependency update,
classifier change or required-check change is part of this reduction. Any future
mobile/shared-contract change must pass the existing mobile security checks.

The retrospective detector core, evaluator and original measurement contract are
unchanged. Previously measured results remain descriptive final-bar replay; this
release makes no efficacy or profitability claim.
