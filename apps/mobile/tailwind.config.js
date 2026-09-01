// New as of the NativeWind redesign. Color values below are a literal,
// hand-copied duplicate of src/theme.ts's own `colors` object -- NOT a
// live import. tailwind.config.js is loaded by Metro's build step under
// plain Node, which can't require() a .ts file without ts-node/register
// overhead this project's toolchain doesn't have, so this file has to
// carry its own copy of the same values. This mirrors theme.ts's own
// documented relationship to the web app's CSS custom properties (also
// a hand-kept-in-sync literal copy, not a shared import, for the same
// cross-runtime reason) -- update BOTH files if a token value changes.
/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./App.tsx", "./src/**/*.{ts,tsx}"],
  presets: [require("nativewind/preset")],
  theme: {
    extend: {
      colors: {
        background: "#16171d", // --bg
        panel: "#1c1d24", // --bg-panel
        row: "#22232b", // --bg-row
        border: "#2e303a", // --border
        text: "#f3f4f6", // --text-h (headline/primary text)
        muted: "#9ca3af", // --text (secondary text)
        accent: "#c084fc", // --accent
        "accent-bg": "rgba(192, 132, 252, 0.15)", // --accent-bg
        good: "#0ca30c", // --good
        "good-bg": "rgba(12, 163, 12, 0.12)", // --good-bg
        warning: "#fab219", // --warning
        "warning-bg": "rgba(250, 178, 25, 0.12)", // --warning-bg
        critical: "#d03b3b", // --critical
        "critical-bg": "rgba(208, 59, 59, 0.14)", // --critical-bg
      },
      fontFamily: {
        // Real, not aspirational -- same reasoning as theme.ts's own
        // monoFont constant: the web app's --mono stack names "JetBrains
        // Mono" but never actually loads the font file anywhere, so this
        // matches that actual behavior (system monospace) rather than
        // bundling a font neither platform's reference actually uses.
        mono: ["monospace"],
      },
    },
  },
  plugins: [],
};
