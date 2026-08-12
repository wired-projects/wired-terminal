interface StatusPillProps {
  label: string;
  /** null = unknown (still probing), true = up, false = down. */
  state: boolean | null;
  /** Overrides the state-derived colour, e.g. a muted dot for "idle". */
  color?: string;
  title?: string;
}

const STATE_COLOR: Record<string, string> = {
  null: 'var(--warning)',
  true: 'var(--success)',
  false: 'var(--danger)',
};

export function StatusPill({ label, state, color, title }: StatusPillProps) {
  const dot = color ?? STATE_COLOR[String(state)];
  return (
    <span className="pill" title={title}>
      <span
        className="pill-dot"
        style={{ '--dot': dot } as React.CSSProperties}
        data-glow={state === true}
        data-pulse={state === null}
      />
      {label}
    </span>
  );
}
