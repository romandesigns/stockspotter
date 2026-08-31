// Real icon paths lifted from the Artifact prototype's own SVG sprite
// (stockspotter-super-chart-prototype memory) — #i-layers, #i-sliders,
// #i-bolt, #i-expand — not new icon designs, so the toolbar reads the
// same way the prototype's own icon-only buttons did rather than the
// text labels ("Indicators"/"Settings"/"Alerts"/"Full screen") used as a
// placeholder in earlier rounds.

const PATHS = {
  layers: (
    <>
      <path fill="currentColor" d="M12 2 2.5 7.5 12 13l9.5-5.5z" />
      <path fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" d="M2.5 12L12 17.5 21.5 12" />
      <path fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" d="M2.5 16.5L12 22l9.5-5.5" />
    </>
  ),
  sliders: (
    <>
      <g stroke="currentColor" strokeWidth={1.6} fill="none" strokeLinecap="round">
        <line x1="4" y1="6" x2="20" y2="6" />
        <line x1="4" y1="12" x2="20" y2="12" />
        <line x1="4" y1="18" x2="20" y2="18" />
      </g>
      <circle cx="9" cy="6" r="2.1" fill="currentColor" />
      <circle cx="16" cy="12" r="2.1" fill="currentColor" />
      <circle cx="7.5" cy="18" r="2.1" fill="currentColor" />
    </>
  ),
  bolt: <path fill="currentColor" d="M13 2 3 14h7l-1 8 11-14h-7z" />,
  expand: (
    <path
      fill="currentColor"
      d="M4 4h6v2H6.4L10 9.6 8.6 11 5 7.4V10H4zM20 4v6h-2V6.4L14.4 10 13 8.6 16.6 5H14V4zM4 20v-6h2v3.6L9.6 14 11 15.4 7.4 19H10v2zM20 20h-6v-2h3.6L14 14.4l1.4-1.4 3.6 3.6V13h2z"
    />
  ),
  // The prototype's own sprite only ever had one fullscreen icon (its
  // demo button was a one-way "expand", never a real toggle) -- a real
  // exit-fullscreen state needs a distinct icon, so this one is a
  // deliberate addition, not lifted from the prototype like the others.
  collapse: <path fill="currentColor" d="M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5zm2-11V5h-2v5h5V8h-3z" />,
} as const;

export type ChartIconName = keyof typeof PATHS;

export function ChartIcon(props: { name: ChartIconName }) {
  return (
    <svg className="chart-icon-svg" viewBox="0 0 24 24" width={14} height={14} aria-hidden="true">
      {PATHS[props.name]}
    </svg>
  );
}
