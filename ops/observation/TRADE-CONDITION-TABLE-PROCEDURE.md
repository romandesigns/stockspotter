# Trade-condition table — acquisition procedure (Step 4B-main §12)

**Status (2026-09-28, Step 4B-main.1): steps 1-6 EXECUTED under GPT/user
authorization; step 7 (bind into the final preregistration) awaits the freeze.**
The authorized one-time fetch of static condition metadata ran at
2026-09-28T21:54:00Z. No symbol, trade, bar or outcome was requested. The
results:

- raw bodies, byte-exact (`-text` in `.gitattributes`), in `metadata/`:

  | Tape | SHA-256 | Bytes |
  |---|---|---|
  | A | `fa1bee5a96f152b2069fb413cd44bc99a7c8a44edd6439f1c38b67e183ad4709` | 810 |
  | B | identical to A | 810 |
  | C | `c0bbaaefbb8925e5e64a25ef0befe582144954fa29aa788796b8a96cf441cb6c` | 850 |

  URLs, timestamps and HTTP status are in `metadata/SNAPSHOT.tsv`;
- **the tapes disagree materially** (step 4):
  - `B` is Average Price on CTA but Bunched Trade on UTP;
  - `E` is Automatic Execution on CTA but a placeholder on UTP;
  - `8` is Reserved on CTA but a 611-exempt placeholder on UTP;
  - regular sale is `" "` (space) on CTA and `@` on UTP.

  The table is therefore **per tape** (`trade-condition-policy-v2`);
- the proposed classification, with a rationale per code, is in
  `trade-conditions-v2.proposed.json` (PROPOSED-NOT-FROZEN). A test proves
  that it classifies exactly the fetched codes per tape and that the snapshot
  bytes are unchanged;
- the trading-status classification is in
  `trading-status-policy-v1.proposed.json`. It is derived from the
  documentation snapshot `metadata/alpaca-doc-real-time-stock-pricing-data-20260928.md`.

The text below is the original procedure, kept for the record.

**Original status: PROCEDURE ONLY.** The table in
`trade-conditions.fixture.json` is a synthetic test fixture. Its SHA binds the
fixture preregistration, not any real policy. The real table has to be
acquired, classified and hashed by the steps below. That has to happen
*before* the preregistration freeze, because the evaluator refuses any table
whose identity differs from `outcome.tradeConditionTableSha256`
(`EvaluationError::PolicyIdentity`).

## Why an external source is needed

The repo has no authoritative condition table. The only condition the
detector interprets is the reopening code `"5"` (halt detection). An
outcome of "a trade printed at ≥ +2 %" is only as meaningful as the rule for
which prints count. Odd-lot, out-of-sequence, average-price and
derivatively-priced prints can sit far from the tradable market. If they are
included by accident, false hits are manufactured.

## Source

Alpaca Market Data v2, condition-code metadata. This is static reference
data, not market data or outcomes:

```
GET https://data.alpaca.markets/v2/stocks/meta/conditions/trade?tape=A
GET https://data.alpaca.markets/v2/stocks/meta/conditions/trade?tape=B
GET https://data.alpaca.markets/v2/stocks/meta/conditions/trade?tape=C
```

Each response maps a code to a description. The fetch touches no symbol, no
trade and no outcome, so it is outside the §-firewall. It still needs explicit
authorization, because it is an external call made with the production
account's credentials.

## Steps

1. **Authorize.** GPT/user approve this one-time metadata fetch.
2. **Fetch** all three tapes from a dev machine (not the VPS). Read the key
   from the dev `.env` into shell variables only, and never echo it. Save the
   raw bodies verbatim as `conditions-trade-tape{A,B,C}-<UTC date>.json`
   under `stockspotter-research/step4b/conditions/`.
3. **Hash the raw snapshots** (SHA-256 of the bytes as received) and record
   them in `conditions/SNAPSHOT.md` with the fetch time, the URL and the HTTP
   status.
4. **Check the tapes agree.** A code whose description differs between tapes
   is listed explicitly. The policy is per-code, so a disagreement is a
   decision item, not something silently resolved.
5. **Classify** every code into exactly one of `included` or `excluded`,
   using the proposal below. Any code left unclassified makes the table
   invalid: `ConditionPolicy::bind` requires disjoint lists, and the
   evaluator censors any trade that carries an unknown code rather than
   guessing.
6. **Write** `trade-conditions-v1.json` (schema `trade-condition-policy-v1`)
   with `sourceSnapshots: [{tape, sha256}]` so that the table names the bytes
   it was derived from.
7. **Bind.** Run `observation_archive.py prereg-sha` on the frozen
   preregistration after putting the table's JCS SHA into
   `outcome.tradeConditionTableSha256`. Then run `ConditionPolicy::bind` on
   the table. Both must agree (the Rust test pattern in
   `observation_main_tests.rs` already exercises this path on the fixture).

## Classification proposal (for GPT review, not adopted)

Principle: **a print counts only if it could have been an execution at the
prevailing market by an ordinary participant at that moment.**

| Class | Typical codes (UTP/CTA meaning) | Proposal | Reason |
|---|---|---|---|
| Regular sale | `@` regular, `F` intermarket sweep, `E` automatic execution | **include** | Ordinary executions at the market |
| Odd lot | `I` | **include** (proposed) | Since 2023 odd lots print inside the NBBO and dominate low-priced names. Excluding them would drop most real prints on the small caps this scanner targets. **Decision item.** |
| Opening/closing/reopening prints | `O`, `Q`, `6`, `M`, `5` | **include** (proposed) | Real auction executions. Note that `5` (reopening) can only land after a halt, and the halt rule already censors horizons overlapping a halt |
| Out of sequence / late | `Z`, `L`, `U` (sold out of sequence, sold last, extended-hours sold) | **exclude** | Timestamp does not reflect when the price was available |
| Average price | `W` | **exclude** | Not a single execution price |
| Derivatively priced / prior reference | `4`, `H`, `P`, `R`, `X`, `N`, `C` (next day, cash), `B` (bunched) | **exclude** | Price is not set by the market at that instant |
| Form T / extended hours | `T` | **exclude** | Outside RTH; the scope rule makes these unreachable anyway |
| Contingent / qualified contingent | `V`, `7` | **exclude** | Priced by a linked instrument |
| Corrections / cancels | (separate message types) | **exclude** | Not trades |

These letters are the *common* SIP meanings. The fetched descriptions are
authoritative wherever they differ, and step 4 lists every such difference.

## Decision items for GPT

1. Authorize the metadata fetch (step 1).
2. Odd lots `I`: include or exclude?
3. Auction prints `O`/`Q`/`6`/`M`/`5`: include or exclude?
4. Should one table apply across tapes A/B/C, or should the table be
   per-tape?
