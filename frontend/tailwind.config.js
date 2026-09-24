/**
 * The palette is declared here and nowhere else; components use it only through
 * Tailwind classes.
 *
 * Neutral greys by default; state colours are muted and reserved for state.
 */
export const palette = {
  ink: { 950: '#0f1012', 900: '#141518', 800: '#1a1b1f', 700: '#222328' },
  line: { DEFAULT: '#2a2c31', soft: '#1f2024' },
  fg: { DEFAULT: '#e2e3e6', muted: '#9b9ea6', faint: '#6c6f78' },
  state: { ok: '#6aa982', warn: '#cfa24e', bad: '#d1706a', info: '#7b9cc4' },
};

export default {
  content: ['./index.html', './src/**/*.{js,jsx}'],
  theme: {
    extend: {
      colors: palette,
      fontFamily: {
        sans: ['-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'Roboto', '"Helvetica Neue"', 'Arial', 'sans-serif'],
        mono: ['ui-monospace', 'SFMono-Regular', 'Menlo', 'monospace'],
      },
      boxShadow: {
        /** Popovers only (menus, tooltips); cards are flat. */
        soft: '0 4px 12px rgba(0,0,0,.35)',
        ring: '0 0 0 2px rgba(123,156,196,.45)',
      },
    },
  },
  plugins: [],
};
