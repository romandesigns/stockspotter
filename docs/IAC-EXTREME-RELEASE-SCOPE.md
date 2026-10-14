# Disabled web/server release scope

The experimental protocol decoder and lifecycle types live only in
`apps/client/src/lib/experimentalStrategies.ts`. The entire shared-types package,
mobile application, root JavaScript manifests and root lockfile are identical to
the reviewed base commit `5807d6116f5c793d6fd62ac255f443620ce21fa8`.

Native delivery remains deferred. The ordinary shared protocol is unchanged;
only an independently opted-in web/desktop research socket can request the new
messages. Backend flags and the frontend feature flag default off. This release
does not activate strategies, capture a cohort or change orders or notifications.

This is a scope reduction, not dependency remediation. Mobile remains blocked by
the braces and node-forge advisories. No advisory waiver, alias, dependency update,
classifier change or required-check change is part of this reduction. Any future
mobile/shared-contract change must pass the existing mobile security checks.

The retrospective detector core, evaluator and original measurement contract are
unchanged. Previously measured results remain descriptive final-bar replay; this
release makes no efficacy or profitability claim.
