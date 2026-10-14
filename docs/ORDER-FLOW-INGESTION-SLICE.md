# Observational ingestion slice

This ports only the order-flow store and three ingestion hook points onto current master. It preserves the server-owned discovery cache, all scanner thresholds, event serialization, IAC/Extreme strategies, orders and notifications. ORDER_FLOW_ENABLED is false unless explicitly set to true or1 at startup. Production was independently checked false during preparation.

Enabled resource policy: at most 256 quote-created engines; sweep idle entries at most once per event-time second after 600 seconds without quote or trade activity; reject and count new symbols when all slots are active. Incoming traffic preserves its existing engine before sweeping. Eviction/recreation produces a partial history, never a full-session assertion. Counters are process-lifetime; day state only advances using the existing 04:00 America/New_York calendar.

Correction/cancel frames are counted, not applied. ObservationHealth is global process telemetry and TapeProvenance is Uncorrected or CorrectionsPending; neither claims per-symbol reconciliation. Trade IDs, tape-specific condition classification and corrected profiles remain deferred. Invalid raw frames do not become authoritative observations.

This slice adds no HTTP route, WS delivery, recorder, backfill caller, bar-cache warmer, halt log or UI. The imported history helpers remain uncalled by runtime and are not cleared for activation. Summary still computes under the global mutex; future delivery must use a cached/sharded lite snapshot and load validation before enabling consumers. Backfill resource/generation bounds, recorder byte ceiling and stale-array semantics remain separate gates. Observational enablement is not authorized by this merge alone.

Validation:202 market-data library tests passed;18 store tests include real enabled/default-off hooks in isolated processes, capacity rejection, idle eviction, late previous-day disposition, corrections/cancels and existing session cumulative-delta regression. Source tests do not prove production throughput or classification efficacy.

Activation additionally requires a sane receipt-time/feed-time window before day/sweep advances, and enabled full-universe ingestion load validation: raw-frame parsing and per-trade global locking are not cleared by these correctness tests. Partial provider history and async backfill generation replacement are also unresolved; no backfill consumer is included.
