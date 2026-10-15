// "Plan" section of the decision card: turns a candidate into numbers
// before anything is committed -- shares for a fixed dollar risk at a
// chosen stop, what the spread costs on top, reward:risk to an optional
// target -- and can log the plan to the trade journal. Arithmetic is
// shared-types' computePlan (same as mobile); form defaults and the entry
// built for POST /journal are lib/journal.ts. Logging records a plan; it
// never places an order.
//
// DecisionCard keys this by symbol, so every input resets when the
// selection changes (the risk amount is re-read from storage).

import type { ShortlistCandidate } from "@stockspotter/shared-types";
import { computePlan } from "@stockspotter/shared-types";
import { useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { formatPrice } from "../../lib/format";
import {
  activeUpperBand, buildJournalEntry, DEFAULT_PLAN_RISK, formatMoney, freshSpreadPct, newJournalId, parseNumberInput,
  planDefaults, readPlanRisk, savePlanRisk,
} from "../../lib/journal";
import { postJournalEntry } from "../../lib/useJournal";

type LogStatus = { kind: "idle" } | { kind: "saving" } | { kind: "saved"; text: string } | { kind: "error"; text: string };

function initialRisk(): string {
  try {
    return String(readPlanRisk(localStorage));
  } catch {
    return String(DEFAULT_PLAN_RISK);
  }
}

function Field(props: { id: string; label: string; hint?: string; value: string; placeholder?: string; onChange: (value: string) => void }) {
  return (
    <div className="plan-field">
      <Label htmlFor={props.id} className="plan-label">
        {props.label}
        {props.hint && <span className="plan-hint">{props.hint}</span>}
      </Label>
      <Input
        id={props.id}
        className="plan-input"
        type="number"
        inputMode="decimal"
        min="0"
        step="any"
        value={props.value}
        placeholder={props.placeholder}
        onChange={(e) => props.onChange(e.target.value)}
      />
    </div>
  );
}

export function PlanCalculator(props: { c: ShortlistCandidate }) {
  const { c } = props;
  const ids = useId();
  const [defaults] = useState(() => planDefaults(c));
  const [entryText, setEntryText] = useState(defaults.entry);
  const [stopText, setStopText] = useState(defaults.stop);
  const [targetText, setTargetText] = useState("");
  const [riskText, setRiskText] = useState(initialRisk);
  const [status, setStatus] = useState<LogStatus>({ kind: "idle" });

  // A logged/failed message describes the inputs it was sent with; editing
  // any input makes it stale, so it clears.
  const edit = (set: (v: string) => void) => (value: string) => {
    set(value);
    if (status.kind !== "saving") setStatus({ kind: "idle" });
  };
  function changeRisk(value: string) {
    edit(setRiskText)(value);
    const n = parseNumberInput(value);
    if (n !== undefined) {
      try { savePlanRisk(localStorage, n); } catch { /* not remembered, still applied */ }
    }
  }

  const entry = parseNumberInput(entryText);
  const stop = parseNumberInput(stopText);
  const target = parseNumberInput(targetText);
  const risk = parseNumberInput(riskText);
  const result = computePlan({
    entry: entry ?? Number.NaN,
    stop: stop ?? Number.NaN,
    riskDollars: risk ?? Number.NaN,
    target,
    spreadPct: freshSpreadPct(c),
    haltUpperBand: activeUpperBand(c),
  });

  async function logTrade() {
    if (!result.ok || entry === undefined || stop === undefined) return;
    setStatus({ kind: "saving" });
    const logged = buildJournalEntry({
      id: newJournalId(),
      candidate: c,
      entry,
      stop,
      target,
      shares: result.plan.shares,
      nowIso: new Date().toISOString(),
    });
    const saved = await postJournalEntry(logged);
    setStatus(
      saved.ok
        ? { kind: "saved", text: `Logged ${logged.shares.toLocaleString("en-US")} ${c.symbol} at ${formatPrice(entry)}. It is in the Journal.` }
        : { kind: "error", text: `Not logged: ${saved.error}` },
    );
  }

  const stopHint = stopText === defaults.stop ? defaults.stopBasis : "edited";
  const entryHint = entryText === defaults.entry ? defaults.entryBasis : "edited";

  return (
    <div className="plan">
      <div className="plan-fields">
        <Field id={`${ids}-entry`} label="Entry" hint={entryHint} value={entryText} onChange={edit(setEntryText)} />
        <Field id={`${ids}-stop`} label="Stop" hint={stopHint} value={stopText} onChange={edit(setStopText)} />
        <Field id={`${ids}-target`} label="Target" hint="optional" value={targetText} placeholder="none" onChange={edit(setTargetText)} />
        <Field id={`${ids}-risk`} label="Risk $" value={riskText} onChange={changeRisk} />
      </div>

      {result.ok ? (
        <>
          <dl className="decision-facts plan-output" aria-live="polite">
            <div className="decision-fact">
              <dt>Shares</dt>
              <dd><span className="price">{result.plan.shares.toLocaleString("en-US")}</span></dd>
            </div>
            <div className="decision-fact">
              <dt>Position</dt>
              <dd><span className="price">{formatMoney(result.plan.positionDollars)}</span></dd>
            </div>
            <div className="decision-fact">
              <dt>Risk per share</dt>
              <dd><span className="price">{formatMoney(result.plan.riskPerShare)}</span></dd>
            </div>
            <div className="decision-fact">
              <dt>Spread cost</dt>
              <dd><span className="price">{freshSpreadPct(c) === undefined ? "not included" : formatMoney(result.plan.spreadCostDollars)}</span></dd>
            </div>
            <div className="decision-fact">
              <dt>Total risk</dt>
              <dd><span className="price">{formatMoney(result.plan.totalRiskDollars)}</span></dd>
            </div>
            <div className="decision-fact">
              <dt>Reward : risk</dt>
              <dd><span className="price">{result.plan.rewardToRisk !== undefined ? `${result.plan.rewardToRisk.toFixed(2)} : 1` : "—"}</span></dd>
            </div>
          </dl>
          {result.plan.warnings.length > 0 && (
            <ul className="decision-reasons plan-warnings">
              {result.plan.warnings.map((w) => <li key={w}>{w}</li>)}
            </ul>
          )}
        </>
      ) : (
        <p className="decision-note decision-note-warning plan-error" aria-live="polite">{result.error}</p>
      )}

      <div className="plan-log">
        <Button size="sm" variant="secondary" disabled={!result.ok || status.kind === "saving" || status.kind === "saved"} onClick={() => void logTrade()}>
          {status.kind === "saving" ? "Logging…" : status.kind === "saved" ? "Logged" : "Log trade"}
        </Button>
        {(status.kind === "saved" || status.kind === "error") && (
          <span className={`plan-status plan-status-${status.kind}`} role="status">{status.text}</span>
        )}
      </div>
      <p className="decision-note">Logging records your plan; it does not place an order.</p>
    </div>
  );
}
