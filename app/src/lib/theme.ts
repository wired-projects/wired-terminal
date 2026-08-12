/**
 * The one place JS is allowed to know a colour.
 *
 * xterm.js paints to a canvas, so it needs literal values rather than CSS
 * custom properties. Everything else in the app styles itself from the tokens
 * in `styles.css` — if you change a colour there and it appears here too, both
 * must move together.
 */

import type { ITheme } from '@xterm/xterm';

export const FONT_MONO =
  "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Monaco, Consolas, monospace";

/** Mirrors --panel / --text / --accent from styles.css. */
export const xtermTheme: ITheme = {
  background: '#0c0a14',
  foreground: '#eeecf5',
  cursor: '#7c5cff',
  cursorAccent: '#0c0a14',
  selectionBackground: 'rgba(124, 92, 255, 0.35)',
  selectionForeground: '#eeecf5',

  black: '#1a1826',
  red: '#f07182',
  green: '#34d399',
  yellow: '#f59e0b',
  blue: '#7c5cff',
  magenta: '#c084fc',
  cyan: '#22d3ee',
  white: '#eeecf5',

  brightBlack: '#605b78',
  brightRed: '#f9a8b0',
  brightGreen: '#6ee7b7',
  brightYellow: '#fbbf24',
  brightBlue: '#a78bfa',
  brightMagenta: '#d8b4fe',
  brightCyan: '#67e8f9',
  brightWhite: '#ffffff',
};

/** Accent colour per provider, for cards and the session toolbar. */
export const PROVIDER_ACCENT: Record<string, string> = {
  claude: 'var(--claude)',
  grok: 'var(--grok)',
  codex: 'var(--codex)',
  gemini: 'var(--gemini)',
  shell: 'var(--accent)',
};
