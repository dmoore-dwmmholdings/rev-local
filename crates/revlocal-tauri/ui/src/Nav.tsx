/**
 * The six screens §15 names, and which one is showing (RL-1105 onward).
 *
 * No router library. Six screens with no URLs to preserve — the window is not a
 * browser tab, there is no back button to honour and nothing to deep-link — so a
 * router would be a dependency bought for its address bar, which this app does
 * not have.
 *
 * §15 requires the kill switch to be reachable from *every* screen. That is why
 * it lives in the header beside this rather than on any screen: a switch that
 * moves with the view is a switch somebody has to find first.
 */
export const SCREENS = [
  'dashboard',
  'repository',
  'run',
  'findings',
  'approvals',
  'settings',
] as const;

export type Screen = (typeof SCREENS)[number];

/** What each tab says. §15's own names, so the UI and the spec read alike. */
export const SCREEN_LABELS: Record<Screen, string> = {
  dashboard: 'Dashboard',
  repository: 'Repository',
  run: 'Run detail',
  findings: 'Findings',
  approvals: 'Approvals',
  settings: 'Settings',
};

/** Screens that are specified but not built yet. */
export const UNBUILT: ReadonlySet<Screen> = new Set<Screen>([
  'repository',
  'findings',
  'settings',
]);

export function Nav({
  screen,
  onSelect,
}: {
  screen: Screen;
  onSelect: (next: Screen) => void;
}) {
  return (
    <nav className="nav" aria-label="screens">
      {SCREENS.map((s) => {
        const unbuilt = UNBUILT.has(s);
        return (
          <button
            key={s}
            className={s === screen ? 'nav-tab nav-current' : 'nav-tab'}
            aria-current={s === screen ? 'page' : undefined}
            disabled={unbuilt}
            // Listed and disabled rather than omitted. §15 names six screens; a
            // nav showing two would make somebody wonder whether the other four
            // exist under a menu somewhere. Disabled says "specified, not built".
            title={unbuilt ? `${SCREEN_LABELS[s]} is not built yet` : SCREEN_LABELS[s]}
            onClick={() => onSelect(s)}
          >
            {SCREEN_LABELS[s]}
          </button>
        );
      })}
    </nav>
  );
}

/**
 * The screen to open on launch (RL-1102, SPEC §16.4).
 *
 * §16.4's own example launches the app "--route /$SCREEN" so a capture harness
 * can photograph one screen at a time. Without something like it, capturing the
 * approvals inbox means driving a click into a webview — and a webview's DOM is
 * not exposed to the OS accessibility APIs that scripted clicking uses, which is
 * an afternoon nobody gets back.
 *
 * Validates whatever the app was told to open. An unknown value falls back to the
 * dashboard rather than failing: a mistyped screen name should not produce a blank
 * window, and this string comes from an environment variable somebody typed.
 */
export function initialScreen(wanted: string): Screen {
  return SCREENS.includes(wanted as Screen) ? (wanted as Screen) : 'dashboard';
}
