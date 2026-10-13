// The app's one real section-header primitive -- extracted verbatim from
// App.tsx's own local `Section` helper (same markup, same classes, same
// optional headerExtra slot) so a new screen section can reuse it
// instead of re-deriving the uppercase/tracked label treatment. Same
// move, and same reason, as empty-state.tsx's extraction of App.tsx's
// local `Empty`.
//
// `count` is the one addition: the web app's own PanelShell already
// renders a row count beside every feed panel's title
// (apps/client/src/components/PanelShell.tsx), and mobile had no
// equivalent because every existing caller passes a slice of fixed
// length. It renders through the exact same headerExtra slot, so a
// caller that passes neither is byte-identical to the old helper.
import * as React from "react";
import { View } from "react-native";
import { Text } from "./text";

export function Section({ title, headerExtra, count, children }: {
  title: string;
  headerExtra?: React.ReactNode;
  count?: number;
  children: React.ReactNode;
}) {
  const extra = count === undefined ? headerExtra : (
    <>
      {headerExtra}
      <Text mono variant="muted" className="text-[10px]">{count}</Text>
    </>
  );
  return (
    <View>
      {extra ? (
        <View className="mb-1.5 flex-row items-center justify-between">
          <Text variant="muted" className="text-[10px] font-semibold uppercase tracking-wider">{title}</Text>
          <View className="flex-row items-center gap-2">{extra}</View>
        </View>
      ) : (
        <Text variant="muted" className="mb-1.5 text-[10px] font-semibold uppercase tracking-wider">{title}</Text>
      )}
      {children}
    </View>
  );
}
