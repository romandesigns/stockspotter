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
  // The Backtest Replay date-range picker's own icons (#i-calendar,
  // #i-chevron-l, #i-chevron-r) -- real paths, not redrawn from the
  // screenshot, for SessionDatePicker.tsx's calendar/month-nav.
  calendar: <path fill="currentColor" d="M7 1.5v2H5A2.5 2.5 0 002.5 6v1.5h19V6A2.5 2.5 0 0019 3.5h-2v-2h-2v2H9v-2zM2.5 9v10A2.5 2.5 0 005 21.5h14a2.5 2.5 0 002.5-2.5V9z" />,
  "chevron-l": <path fill="currentColor" d="M15 4l-8 8 8 8 1.4-1.4L9.8 12l6.6-6.6z" />,
  "chevron-r": <path fill="currentColor" d="M9 4l8 8-8 8-1.4-1.4L14.2 12 7.6 5.4z" />,
  // The prototype's own Backtest Replay tab icon (#i-replay) -- real
  // path, for the left nav rail's launcher (App.tsx).
  replay: <path fill="currentColor" d="M12 5V2L7 6l5 4V7a5 5 0 11-4.9 6H4.98A7 7 0 1012 5z" />,
  // The prototype's own Backtest Replay playback-control icons --
  // #i-play/#i-pause/#i-back/#i-fwd, for ReplayLauncher.tsx.
  play: <path fill="currentColor" d="M8 5v14l11-7z" />,
  pause: <path fill="currentColor" d="M7 5h4v14H7zM13 5h4v14h-4z" />,
  back: <path fill="currentColor" d="M18 5v14l-9-7zM6 5h2v14H6z" />,
  fwd: <path fill="currentColor" d="M6 5v14l9-7zM16 5h2v14h-2z" />,
  // The prototype's own Sessions-filter icon (#i-daynight), for
  // ReplayLauncher.tsx's pre/regular/after-hours toggle.
  daynight: (
    <>
      <path fill="currentColor" transform="translate(0.2,8.2) scale(0.6)" d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z" />
      <circle cx="16.6" cy="7.2" r="2.1" fill="currentColor" />
      <g stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
        <line x1="16.6" y1="2.6" x2="16.6" y2="3.7" />
        <line x1="21.2" y1="7.2" x2="20.1" y2="7.2" />
        <line x1="19.6" y1="4.2" x2="18.85" y2="4.95" />
      </g>
    </>
  ),
} as const;

export type ChartIconName = keyof typeof PATHS;

export function ChartIcon(props: { name: ChartIconName }) {
  return (
    <svg className="chart-icon-svg" viewBox="0 0 24 24" width={14} height={14} aria-hidden="true">
      {PATHS[props.name]}
    </svg>
  );
}
