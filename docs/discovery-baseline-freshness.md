# Live discovery baseline recovery

Daily volume seeds used by the Gap & Go scanner are accepted only if the latest returned completed bar matches the prior scheduled regular session, corroborated by snapshot dates. The baseline day, REST window end, expected session and cache rollover all use the New York calendar date. This is separate from the 04:00 trading-day boundary used for session volume.

A missing bar is unknown data, never synthetic zero volume. An explicitly returned zero-volume bar remains valid input. The existing integer mean of up to twenty returned completed bars, prior-close calculation and 5x threshold are unchanged on complete inputs. Historical replay uses its unchanged seed API.

Incomplete seeds are retried after 5, 10, 20, 40 and then 60 minutes, up to twelve attempts per symbol per calendar day. The first provider-error or interrupted-request retry is after 30 seconds. Attempts are reserved before HTTP starts and finalised without spending them twice. A batch includes up to 100 symbols, with due retries filling unused space; first and retry budgets are independent (240 and 12 batches per rolling hour). Each fetch permits at most four HTTP pages, with redirects disabled and partial pagination rejected. The conservative combined upper bound is 1,008 HTTP requests per rolling hour; ordinary batches usually require one page. Both initial requests and retries are included.

The server owns FloatCache above its feed reconnect loop. Accepted seed values, input hashes, retry state and FMP budget survive a feed reconnect. Current-session volume state is rebuilt as before. A process restart can refetch; this candidate does not persist state to disk or automatically reconcile earlier-process hashes.

Audit records contain the expected/latest/window-start dates, actual bar count, fetch time, feed, raw adjustment, input hash, accepted values and status/reason. The two new baseline audit classes are critical and have explicit loss-mask bits. Calendar/snapshot disagreements include witness counts by date. The existing shared/mobile wire protocol remains byte-identical. On-screen incomplete-evaluation diagnostics are a separate, mobile-gated follow-up; until then baseline status is available in audit records.

Scanner price ($0.25-$20) and float (at most 20M) checks remain distinct from the display-only Ross badge ($1-$20, float below 10M, plus news). Labels make that distinction explicit; neither rule is relaxed.

Exceptional exchange closures absent from the existing calendar fail closed when observed dates disagree. There is no automatic override. Sparse-history windows still use returned bars, not inferred trading sessions. Mover-only, confirmed-watch and managed-position trackers retain their existing seed API; this change governs live discovery qualification.

The retry schedule and budgets are conservative starting policies, not measured optimal settings. Local mock-provider tests establish correctness under late/missing data and recovery, not provider availability, trading efficacy or production deployment readiness.
