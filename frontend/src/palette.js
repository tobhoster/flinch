// The palette from index.css's `@theme`, as values, for the graph and the
// rules canvas, which compute and mix colours. main.jsx imports index.css
// before anything that imports this module, so the variables are set by the
// time it evaluates.
const style = getComputedStyle(document.documentElement);
const color = (name) => style.getPropertyValue(`--color-${name}`).trim();

export const palette = {
  ink: { 950: color('ink-950'), 900: color('ink-900'), 800: color('ink-800'), 700: color('ink-700') },
  line: { DEFAULT: color('line'), soft: color('line-soft') },
  fg: { DEFAULT: color('fg'), muted: color('fg-muted'), faint: color('fg-faint') },
  state: { ok: color('state-ok'), warn: color('state-warn'), bad: color('state-bad'), info: color('state-info') },
};
