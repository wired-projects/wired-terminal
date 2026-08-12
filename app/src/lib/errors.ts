/**
 * The error map: symptom → one sentence → one button.
 *
 * Written once and used everywhere, because the failures a non-coder meets are
 * always the same handful and every one of them used to end in a shell command
 * he had nowhere to type. Nothing in here mentions a project root, a port
 * number or an environment variable.
 */

import { ApiError, OFFLINE } from './api';

/** What the single button should do. `App` decides how. */
export type Remedy =
  | 'retry'
  | 'restart-assistant'
  | 'open-setup'
  | 'open-settings'
  | 'open-help'
  | 'open-logs';

export interface Guidance {
  title: string;
  detail: string;
  action?: { label: string; remedy: Remedy };
}

const RULES: { match: RegExp; guidance: Guidance }[] = [
  {
    match: /^offline$/,
    guidance: {
      title: "Wired isn't running",
      detail: 'The part that talks to your assistant has stopped answering.',
      action: { label: 'Try again', remedy: 'retry' },
    },
  },
  {
    match: /already in use|address in use/i,
    guidance: {
      title: 'Something else is in the way',
      detail: 'Another program is using the connection Wired needs.',
      action: { label: 'Open diagnostics', remedy: 'open-help' },
    },
  },
  {
    match: /not found|isn't installed|not installed/i,
    guidance: {
      title: 'Your assistant needs setting up',
      detail: "The assistant program isn't on this computer yet. Wired can install it.",
      action: { label: 'Set it up', remedy: 'open-setup' },
    },
  },
  {
    match: /no active (agent|pty) session|isn't running/i,
    guidance: {
      title: 'Your assistant is stopped',
      detail: 'Start it and it will pick up where it left off.',
      action: { label: 'Start it', remedy: 'restart-assistant' },
    },
  },
  {
    match: /can't write|could not use|permission/i,
    guidance: {
      title: "Wired can't use that folder",
      detail: 'Pick a different folder for your assistant to work in.',
      action: { label: 'Choose a folder', remedy: 'open-settings' },
    },
  },
  {
    match: /missing or invalid token|unauthorized/i,
    guidance: {
      title: 'Wired needs the password again',
      detail: 'The access code this window is using no longer matches.',
      action: { label: 'Open settings', remedy: 'open-settings' },
    },
  },
  {
    match: /origin not allowed/i,
    guidance: {
      title: 'This window is not allowed in',
      detail: 'Wired only accepts its own app and the local dev server.',
      action: { label: 'Open diagnostics', remedy: 'open-help' },
    },
  },
  {
    match: /taking longer/i,
    guidance: {
      title: 'Still waiting',
      detail: 'Your assistant is busy or thinking. Give it a moment.',
      action: { label: 'Try again', remedy: 'retry' },
    },
  },
];

export function explain(error: unknown): Guidance {
  const message = error instanceof ApiError ? error.message : String(error ?? '');
  if (message === OFFLINE) return RULES[0].guidance;

  for (const rule of RULES) {
    if (rule.match.test(message)) return rule.guidance;
  }
  return {
    title: 'That did not work',
    // The backend's own sentence is usually the most specific thing available,
    // and it is written for a person.
    detail: message || 'Something went wrong.',
    action: { label: 'Open diagnostics', remedy: 'open-help' },
  };
}

/** One-line form, for a toast. */
export function briefly(error: unknown): string {
  const { title, detail } = explain(error);
  return detail.startsWith(title) ? detail : `${title} — ${detail}`;
}
