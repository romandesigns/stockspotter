// Real shared wrapper (2026-09-03) for the Modal+backdrop+sheet-panel
// structure ChartIndicatorsSheet/ChartSettingsSheet/ChartAlertsSheet
// each independently hand-duplicated (identical backdrop/sheet
// StyleSheet blocks, copy-pasted three times) -- factored out per
// Roman's own "consolidate... clean UI design" ask and this project's
// established "reuse, don't re-derive" discipline
// ([[feedback-reuse-dont-rederive]]). Content scrolls inside the sheet
// when it's tall (a real bottom sheet is expected to scroll -- this is
// unrelated to the Chart Page's own "no vertical scroll" layout
// constraint, which is about the page behind the sheet, not the sheet
// itself).
import { Modal, Pressable, ScrollView, StyleSheet, View } from "react-native";
import { colors } from "../../theme";

export function BottomSheet(props: { visible: boolean; onClose: () => void; closeLabel: string; maxHeightPct?: number; children: React.ReactNode }) {
  return (
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose}>
      <Pressable style={styles.backdrop} onPress={props.onClose} accessibilityRole="button" accessibilityLabel={props.closeLabel} />
      <View style={[styles.sheet, { maxHeight: `${props.maxHeightPct ?? 80}%` }]}>
        <View style={styles.handle} />
        <ScrollView bounces={false}>{props.children}</ScrollView>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: { position: "absolute", left: 0, right: 0, top: 0, bottom: 0, backgroundColor: "rgba(0,0,0,0.5)" },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: colors.row, borderTopLeftRadius: 16, borderTopRightRadius: 16 },
  handle: { alignSelf: "center", width: 36, height: 4, borderRadius: 2, backgroundColor: colors.divider, marginTop: 8, marginBottom: 2 },
});
