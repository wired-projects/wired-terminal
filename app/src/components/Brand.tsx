/**
 * Wired Terminal brand marks.
 *
 * Geometry is shared with the sibling Wired products; this repo's identity is
 * the violet plate plus the terminal-prompt badge. Keep in sync with
 * `brand/logo.svg` — see `brand/README.md`.
 */

interface MarkProps {
  size?: number;
  className?: string;
}

/** Monochrome wire-W. Inherits `currentColor` — use anywhere in the UI. */
export function WiredMark({ size = 20, className }: MarkProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 100 100"
      fill="none"
      className={className}
      aria-hidden="true"
      focusable="false"
    >
      <path
        d="M12 52 C22 52 25 82 38 82 C47 82 48 22 50 22 C52 22 53 82 62 82 C75 82 78 52 88 52"
        stroke="currentColor"
        strokeWidth="8.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="12" cy="52" r="6.5" fill="currentColor" />
      <circle cx="50" cy="22" r="6.5" fill="currentColor" />
      <circle cx="88" cy="52" r="6.5" fill="currentColor" />
    </svg>
  );
}

/** Full app icon: violet plate, glowing mark, terminal badge. */
export function WiredAppIcon({ size = 28, className }: MarkProps) {
  // Gradient/filter ids are namespaced so two icons on one page cannot collide.
  const uid = `wired-${size}`;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 100 100"
      fill="none"
      className={className}
      role="img"
      aria-label="Wired Terminal"
    >
      <defs>
        <linearGradient id={`${uid}-bg`} x1="18" y1="0" x2="82" y2="100" gradientUnits="userSpaceOnUse">
          <stop stopColor="#2e1a5f" />
          <stop offset="1" stopColor="#0a0718" />
        </linearGradient>
        <linearGradient id={`${uid}-wire`} x1="18" y1="35" x2="82" y2="65" gradientUnits="userSpaceOnUse">
          <stop stopColor="#ffffff" />
          <stop offset="0.45" stopColor="#c9b8ff" />
          <stop offset="1" stopColor="#ffffff" />
        </linearGradient>
      </defs>
      <rect width="100" height="100" rx="22.3" fill={`url(#${uid}-bg)`} />
      <rect
        x="0.7"
        y="0.7"
        width="98.6"
        height="98.6"
        rx="21.7"
        stroke="rgba(255,255,255,0.14)"
        strokeWidth="1.1"
      />
      <path
        d="M18 47 C26 47 28 76 38 76 C46 76 47 26 50 26 C53 26 54 76 62 76 C72 76 74 47 82 47"
        stroke={`url(#${uid}-wire)`}
        strokeWidth="6.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="18" cy="47" r="4.6" fill="#f1ecff" />
      <circle cx="50" cy="26" r="4.6" fill="#f1ecff" />
      <circle cx="82" cy="47" r="4.6" fill="#f1ecff" />
      <g transform="translate(78 22)">
        <circle r="14.5" fill="#180d3a" stroke="rgba(255,255,255,0.22)" strokeWidth="1.1" />
        <rect x="-9" y="-7.6" width="18" height="15.2" rx="2.4" fill="#f1ecff" />
        <line x1="-9" y1="-4.2" x2="9" y2="-4.2" stroke="#2e1a5f" strokeWidth="1.1" opacity="0.38" />
        <path
          d="M-5.1 -1 L-2.5 1.5 L-5.1 4"
          stroke="#2e1a5f"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        <line x1="-0.6" y1="4" x2="5.2" y2="4" stroke="#2e1a5f" strokeWidth="1.6" strokeLinecap="round" />
      </g>
    </svg>
  );
}

/** Icon + wordmark, as used in the app header. */
export function WiredWordmark() {
  return (
    <div className="brand-lockup">
      <WiredAppIcon size={26} />
      <span className="brand-name">WIRED</span>
      <span className="brand-sub">TERMINAL</span>
    </div>
  );
}
