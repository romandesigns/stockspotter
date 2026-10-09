# Dashboard display corrections

Current Top Gainers and Highly Trading show internally coherent rows from
the same successful universe snapshot, refreshed at the existing 60-second
scan cadence. Updated age uses that observation time rather than HTTP poll
arrival. The optional 24h peak view shows the recorded peak snapshot and
its date/time. These are separate readings, not interchangeable live quotes.
The existing rolling-peak lists still drive the scanner watchlist; detector
coverage and the mobile client's existing list semantics are unchanged.
Current lists and observation timestamps are additive HTTP response fields.

Chart price remains the latest minute-bar close. Day change uses the prior
close encoded by a fresh same-day scanner/current-snapshot price and percent
pair. Scanner is preferred initially; the first accepted reference is held
for that symbol and New York day to prevent source switching. Zero percent
is ambiguous on older snapshot code and cannot prove a reference. Without
an accepted reference the header and tooltip explicitly show change since
the first loaded candle. This is not a new authoritative prior-close API.

Delayed earlier history reframes a chart only before wheel, drag or touch
navigation. Ordinary ticks do not refit; symbol and explicit timeframe
choices reset the navigation guard. Volume remains an overlay, with its
last-value label and horizontal price line hidden.

Ignition delivery filters replayed confirmations older than 120 seconds,
preserves the 15-minute symbol cooldown, and coalesces separate frames over
one second. At most three toasts are visible. Additional confirmations are
counted in a button that opens the Ignition feed. Audio/browser notices are
grouped at eight-second intervals, with a trailing digest rather than lost
alerts. Dedup/cooldown state and timers are bounded/cleaned up. Web delivery
now differs from the unchanged mobile hook; the shared eligibility rule is
unchanged. Browser notifications require already-granted permission.

AI Read, broker services, frozen research and Android release inputs are
unchanged. Tests cover replay age, cooldown, burst delivery, delayed history,
manual navigation, reference precedence/day rollover, and current/peak
ranking separation. Browser lifecycle tests use a recording chart engine;
a separate actual-engine browser check verifies delayed-history framing, viewport preservation and hidden volume labels/lines. Neither establishes trading efficacy.

