import type { ProviderId } from '../lib/api';

export interface ProviderMeta {
  id: ProviderId;
  label: string;
  blurb: string;
  accent: string;
  isAgent: boolean;
}

export const PROVIDERS: ProviderMeta[] = [
  {
    id: 'claude',
    label: 'Claude Code',
    blurb: 'Agent CLI · always-on ready',
    accent: 'var(--claude)',
    isAgent: true,
  },
  {
    id: 'grok',
    label: 'Grok CLI',
    blurb: 'Agent CLI · always-on ready',
    accent: 'var(--grok)',
    isAgent: true,
  },
  {
    id: 'codex',
    label: 'Codex CLI',
    blurb: 'Agent CLI · always-on ready',
    accent: 'var(--codex)',
    isAgent: true,
  },
  {
    id: 'gemini',
    label: 'Gemini CLI',
    blurb: 'Agent CLI · always-on ready',
    accent: 'var(--gemini)',
    isAgent: true,
  },
  {
    id: 'shell',
    label: 'System Shell',
    blurb: 'Plain terminal (not an agent)',
    accent: 'var(--accent)',
    isAgent: false,
  },
];

export function providerMeta(id: ProviderId | null | undefined): ProviderMeta | undefined {
  return PROVIDERS.find((p) => p.id === id);
}

export function ProviderIcon({ id, size = 22 }: { id: ProviderId; size?: number }) {
  const common = {
    width: size,
    height: size,
    viewBox: '0 0 24 24',
    fill: 'none',
    'aria-hidden': true,
    focusable: 'false' as const,
  };

  if (id === 'claude') {
    return (
      <svg {...common}>
        <circle cx="12" cy="12" r="8" stroke="currentColor" strokeWidth="1.8" />
        <path d="M9 12h6M12 9v6" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    );
  }

  if (id === 'grok') {
    return (
      <svg {...common}>
        <path
          d="M13 3L5 14h6l-1 7 9-12h-6l0-6z"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinejoin="round"
        />
      </svg>
    );
  }

  if (id === 'codex') {
    return (
      <svg {...common}>
        <path
          d="M12 3l7.4 4.3v8.6L12 20.2 4.6 15.9V7.3L12 3z"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinejoin="round"
        />
      </svg>
    );
  }

  if (id === 'gemini') {
    return (
      <svg {...common}>
        <path
          d="M12 2.6c.6 4.7 4.7 8.8 9.4 9.4-4.7.6-8.8 4.7-9.4 9.4-.6-4.7-4.7-8.8-9.4-9.4 4.7-.6 8.8-4.7 9.4-9.4z"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinejoin="round"
        />
      </svg>
    );
  }

  return (
    <svg {...common}>
      <path
        d="M4 6l6 6-6 6M12 18h8"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
