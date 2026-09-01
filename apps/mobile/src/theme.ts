// The real stockspotter design tokens -- hand-ported from
// apps/client/src/index.css's own `:root` block (the actual established
// palette, not a fresh mobile-only approximation). Kept as literal
// values here rather than a shared import because Expo/React Native
// can't consume a web CSS file directly; if the web palette ever
// changes, update both -- these are meant to be the exact same hex
// values, not "close enough".
export const colors = {
  background: "#16171d", // --bg
  surface: "#1c1d24", // --bg-panel
  row: "#22232b", // --bg-row
  divider: "#2e303a", // --border
  text: "#f3f4f6", // --text-h (headline/primary text)
  muted: "#9ca3af", // --text (secondary text)
  dim: "#9ca3af", // no separate --dim on the web side; opacity is used there instead
  accent: "#c084fc", // --accent
  accentBg: "rgba(192, 132, 252, 0.15)", // --accent-bg
  good: "#0ca30c", // --good
  goodBg: "rgba(12, 163, 12, 0.12)", // --good-bg
  warning: "#fab219", // --warning
  warningBg: "rgba(250, 178, 25, 0.12)", // --warning-bg
  critical: "#d03b3b", // --critical
  criticalBg: "rgba(208, 59, 59, 0.14)", // --critical-bg
} as const;

/** Real, not aspirational: the web app's own --mono stack names
 * "JetBrains Mono" but never actually loads the font file anywhere
 * (confirmed -- no @font-face, no Google Fonts <link>, no bundled
 * font package in apps/client), so it silently falls through to
 * whatever monospace font the OS already has. Matching that actual
 * behavior here rather than bundling a font on mobile that the
 * reference platform doesn't really use either -- a real follow-up if
 * this ever gets fixed on the web side too, not a mobile-specific gap. */
export const monoFont = "monospace";
