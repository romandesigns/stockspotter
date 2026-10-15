// Journal half of the Shortlist tab: the trader's own logged plans and
// results (GET/POST /journal, shared with desktop). Closing a trade and
// editing notes both re-POST the same id. Nothing here places an order.
import { useState } from "react";
import { ActivityIndicator, TextInput, View } from "react-native";
import type { JournalEntry } from "@stockspotter/shared-types";
import { journalResult, summarizeJournal } from "@stockspotter/shared-types";
import { Badge } from "../ui/badge";
import { Card } from "../ui/card";
import { Text } from "../ui/text";
import { colors } from "../../theme";
import {
  TRADABILITY_LABEL,
  closeJournalEntry,
  formatEtDateTime,
  formatR,
  formatRvol,
  formatShortlistPrice,
  formatSignedDollars,
  formatSignedPct,
  parsePositive,
  priceInput,
  updateJournalNotes,
} from "../../shortlist";
import type { TradeJournalState } from "../../useTradeJournal";
import { ActionButton, Field, Note } from "./parts";

export function JournalView({ journal }: { journal: TradeJournalState }) {
  const entries = journal.entries;
  return (
    <View className="gap-3">
      <View className="flex-row items-center justify-between">
        <View className="flex-1 flex-row items-center gap-2">
          <Text variant="muted" className="text-[10px] font-semibold uppercase tracking-wider">Your trades</Text>
          {/* Same footprint loading or not, so the row never jumps. */}
          <View style={{ width: 16 }}>{journal.loading && <ActivityIndicator size="small" color={colors.accent} />}</View>
        </View>
        <ActionButton label="Refresh" tone="outline" onPress={journal.refresh} disabled={journal.loading} accessibilityLabel="Refresh journal" />
      </View>
      <Text variant="warning" numberOfLines={1} className="min-h-[15px] text-[10px] opacity-80">
        {journal.error ? `Couldn't load the journal (${journal.error})${entries ? " — showing the last copy" : ""}` : " "}
      </Text>

      {entries === null ? (
        journal.loading ? null : <Text variant="muted" className="py-7 text-center text-xs">Journal unavailable. Tap Refresh to try again.</Text>
      ) : (
        <>
          <Summary entries={entries} />
          {entries.length === 0 ? (
            <Text variant="muted" className="py-7 text-center text-xs">Nothing logged yet. Open a candidate under Today and use Log trade in its Plan.</Text>
          ) : (
            <View className="gap-1.5">
              {entries.map((e) => (
                <EntryCard key={e.id} entry={e} save={journal.save} />
              ))}
            </View>
          )}
        </>
      )}

      <Text variant="muted" className="pt-1 text-center text-[11px]">
        Your own record. Compare it with each card&apos;s track record before trusting any setup.
      </Text>
    </View>
  );
}

function Summary({ entries }: { entries: JournalEntry[] }) {
  const s = summarizeJournal(entries);
  return (
    <Card className="gap-2 p-3">
      <View className="flex-row justify-between">
        <Stat label="Trades" value={String(s.trades)} />
        <Stat label="Closed" value={String(s.closed)} />
        <Stat label="Wins" value={String(s.wins)} />
        <Stat label="Win rate" value={s.winRate === undefined ? "—" : `${Math.round(s.winRate * 100)}%`} />
      </View>
      {s.closed === 0 ? (
        <Note>No closed trades yet</Note>
      ) : (
        <View className="flex-row justify-between">
          <Stat label="Total P&L" value={formatSignedDollars(s.totalPnlDollars)} color={s.totalPnlDollars >= 0 ? colors.good : colors.critical} />
          <Stat label="Average R" value={formatR(s.averageR)} color={(s.averageR ?? 0) >= 0 ? colors.good : colors.critical} />
          <View className="flex-1" />
        </View>
      )}
    </Card>
  );
}

function Stat({ label, value, color }: { label: string; value: string; color?: string }) {
  return (
    <View className="flex-1 gap-0.5">
      <Text mono className="text-sm font-bold" style={color ? { color } : undefined}>{value}</Text>
      <Text variant="muted" className="text-[10px] uppercase">{label}</Text>
    </View>
  );
}

type Editing = { kind: "none" } | { kind: "close"; draft: string } | { kind: "notes"; draft: string };

function EntryCard({ entry, save }: { entry: JournalEntry; save: TradeJournalState["save"] }) {
  const [editing, setEditing] = useState<Editing>({ kind: "none" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const result = journalResult(entry);
  const snap = entry.snapshot;

  const status = !result
    ? { label: "Open", variant: "default" as const }
    : result.pnlDollars > 0
      ? { label: "Win", variant: "good" as const }
      : result.pnlDollars < 0
        ? { label: "Loss", variant: "critical" as const }
        : { label: "Flat", variant: "muted" as const };

  async function submit(next: JournalEntry) {
    setBusy(true);
    setError(null);
    const r = await save(next);
    setBusy(false);
    if (r.ok) setEditing({ kind: "none" });
    else setError(r.error);
  }

  const exitPrice = editing.kind === "close" ? parsePositive(editing.draft) : Number.NaN;

  return (
    <Card className="gap-1.5 p-3">
      <View className="flex-row items-center gap-2">
        <Text mono className="text-[15px] font-bold">{entry.symbol}</Text>
        <Badge variant={status.variant}>{status.label}</Badge>
        <View className="flex-1" />
        {result && (
          <Text mono className="text-xs font-semibold" style={{ color: result.pnlDollars >= 0 ? colors.good : colors.critical }}>
            {formatSignedDollars(result.pnlDollars)} · {formatR(result.rMultiple)}
          </Text>
        )}
      </View>
      <Text mono className="text-xs">
        {entry.shares} sh · entry {formatShortlistPrice(entry.entry)} · stop {formatShortlistPrice(entry.stop)}
        {entry.target !== undefined ? ` · target ${formatShortlistPrice(entry.target)}` : ""}
      </Text>
      <Text variant="muted" className="text-[11px]">
        Logged {formatEtDateTime(entry.createdAt)}
        {entry.exit ? ` · closed at ${formatShortlistPrice(entry.exit.price)}, ${formatEtDateTime(entry.exit.at)}` : ""}
      </Text>
      {snap && (
        <Text variant="muted" className="text-[11px]">
          Card then: {snap.rank !== undefined ? `#${snap.rank} · ` : ""}{formatSignedPct(snap.gapPct)} · {formatRvol(snap.relativeVolume)} RVOL
          {snap.spreadPct !== undefined ? ` · spread ${snap.spreadPct.toFixed(2)}%` : ""}
          {snap.tradability ? ` · ${TRADABILITY_LABEL[snap.tradability].toLowerCase()}` : ""}
          {snap.profile ? ` · ${snap.profile}` : ""}
          {snap.targetRate !== undefined ? ` (${Math.round(snap.targetRate * 100)}% reached target first historically)` : ""}
        </Text>
      )}
      {entry.notes && editing.kind !== "notes" && <Text className="text-xs leading-4">{entry.notes}</Text>}

      {editing.kind === "close" && (
        <View className="flex-row items-end gap-2 pt-1">
          <Field
            className="flex-1"
            label="Exit price"
            value={editing.draft}
            onChangeText={(t) => setEditing({ kind: "close", draft: t })}
            placeholder={`entry was ${priceInput(entry.entry)}`}
            autoFocus
            onSubmitEditing={() => { if (Number.isFinite(exitPrice)) void submit(closeJournalEntry(entry, exitPrice, new Date())); }}
          />
          <View className="pb-[18px]">
            <ActionButton
              label={busy ? "Saving…" : "Close trade"}
              disabled={busy || !Number.isFinite(exitPrice)}
              onPress={() => void submit(closeJournalEntry(entry, exitPrice, new Date()))}
            />
          </View>
        </View>
      )}
      {editing.kind === "notes" && (
        <View className="gap-2 pt-1">
          <TextInput
            value={editing.draft}
            onChangeText={(t) => setEditing({ kind: "notes", draft: t })}
            placeholder="What happened, what you'd do differently"
            placeholderTextColor={colors.muted}
            multiline
            autoFocus
            maxLength={2000}
            accessibilityLabel={`Notes for ${entry.symbol}`}
            style={{ minHeight: 88, textAlignVertical: "top", fontSize: 13, color: colors.text, backgroundColor: colors.surface, borderRadius: 8, borderWidth: 1, borderColor: colors.divider, padding: 10 }}
          />
          <ActionButton label={busy ? "Saving…" : "Save notes"} disabled={busy} onPress={() => void submit(updateJournalNotes(entry, editing.draft, new Date()))} />
        </View>
      )}
      {error && <Note tone="critical">{error}</Note>}

      <View className="flex-row gap-2 pt-1">
        {editing.kind === "none" ? (
          <>
            {!entry.exit && (
              <ActionButton
                label="Close trade…"
                tone="outline"
                className="flex-1"
                onPress={() => { setError(null); setEditing({ kind: "close", draft: "" }); }}
                accessibilityLabel={`Close ${entry.symbol} trade`}
              />
            )}
            <ActionButton
              label={entry.notes ? "Edit notes" : "Add notes"}
              tone="outline"
              className="flex-1"
              onPress={() => { setError(null); setEditing({ kind: "notes", draft: entry.notes ?? "" }); }}
              accessibilityLabel={`Notes for ${entry.symbol}`}
            />
          </>
        ) : (
          <ActionButton label="Cancel" tone="outline" className="flex-1" disabled={busy} onPress={() => { setError(null); setEditing({ kind: "none" }); }} />
        )}
      </View>
    </Card>
  );
}
