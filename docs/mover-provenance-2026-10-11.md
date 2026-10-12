# Mover provenance: informational release

Current mover prices, gap calculations, volume values, ordering and the
rolling-peak scanner watchlist are unchanged. Optional provenance describes
the existing price source, latest provider trade timestamp and snapshot
receipt. A price from a trade with no timestamp remains a trade price with
unknown time; it is not relabelled as a daily-bar fallback.

`observedAt` retains its successful scan-completion meaning. `lastSuccessAt`
and `scanProvenance.scanCompletedAt` use that same clock. `lastAttemptAt`,
`lastErrorAt` and `refreshStatus` describe refresh attempts independently of
the retained successful data. Failed requests or JSON parsing discard the
entire attempted collection; its partial results are never published.
Client polling failure is labelled separately from server refresh failure.

`snapshotReceivedAt` is stamped after each successful response is parsed.
`oldestBatchReceivedAt` describes the oldest response in the successful scan;
it is not an exchange timestamp or a guarantee that every symbol traded then.
`volumeRetrievedAt` is retrieval time for known volume, not proof that volume
is complete through that instant. Null timestamps stay unknown.

The original conversion clock is fixed before the snapshot pass. The
classification market day, daily-bar relation and reference-close date retain
that clock even if the scan crosses New York midnight. Receipt timestamps do
not change those calculations. Daily-bar relation is mapped into explicit
wire strings, without serializing the internal scanner metadata struct.

Coverage counts describe successful provider responses: requested entries,
returned entries, requested entries omitted by the provider, and unique
converted snapshots. They do not measure scanner eligibility or future
outcome completeness. Duplicate requests or unexpected provider symbols can
prevent simple inequalities between these counts. Failed scan coverage is
not published as if it were a successful partial collection.

The interface prints reported trade date/time in New York, daily-bar fallback,
unknown time, or a timestamp ahead of the device clock. It makes no weekday,
holiday or market-open inference and applies no age-based exclusion. Unknown
session volume is displayed as unavailable, not zero. Historical daily rows
and rolling peaks retain their existing semantics. Legacy servers and mobile
clients remain compatible with additive optional fields.

This establishes price provenance and refresh visibility, not trading edge,
full market coverage, latency targets or qualified prospective outcomes.
