/**
 * Inline-SVG-Icons (Stroke-Stil, 16er-Raster). Bewusst ohne Icon-Abhängigkeit:
 * alle Icons erben `currentColor` und skalieren ueber `size`.
 */

interface IconProps {
  size?: number;
}

const SVG_BASE = {
  viewBox: "0 0 16 16",
  "aria-hidden": true,
  focusable: false,
  stroke: "currentColor",
  strokeWidth: 1.5,
  fill: "none",
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

export function PlusIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M8 3v10M3 8h10" />
    </svg>
  );
}

export function MinusIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M3 8h10" />
    </svg>
  );
}

export function ArrowUpIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M8 13V3M4 7l4-4 4 4" />
    </svg>
  );
}

export function ArrowDownIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M8 3v10M4 9l4 4 4-4" />
    </svg>
  );
}

export function EyeIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M1.7 8S4.3 3.8 8 3.8 14.3 8 14.3 8 11.7 12.2 8 12.2 1.7 8 1.7 8Z" />
      <circle cx="8" cy="8" r="1.9" />
    </svg>
  );
}

export function EyeOffIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M2.5 2.5l11 11" />
      <path d="M6.4 6.5C4.1 7.4 1.7 8 1.7 8s2.6 4.2 6.3 4.2c.9 0 1.7-.2 2.4-.5" />
      <path d="M13.6 9.6c.4-.6.7-1.6.7-1.6S11.7 3.8 8 3.8c-.5 0-1 .1-1.4.2" />
    </svg>
  );
}

export function LockIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <rect x="3.5" y="7" width="9" height="6.5" rx="1.2" />
      <path d="M5.5 7V5.2a2.5 2.5 0 0 1 5 0V7" />
    </svg>
  );
}

export function UnlockIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <rect x="3.5" y="7" width="9" height="6.5" rx="1.2" />
      <path d="M5.5 7V5.2A2.5 2.5 0 0 1 10.3 4.4" />
    </svg>
  );
}

export function PowerIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M8 2v5.5" />
      <path d="M11.2 3.8a5 5 0 1 1-6.4 0" />
    </svg>
  );
}

export function SparkleIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M8 1.8l1.3 3.9L13.2 7l-3.9 1.3L8 12.2 6.7 8.3 2.8 7l3.9-1.3L8 1.8Z" />
      <path d="M12.3 10.6l.7 1.9 1.9.7-1.9.7-.7 1.9-.7-1.9-1.9-.7 1.9-.7.7-1.9Z" />
    </svg>
  );
}

export function SourcePlusIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <rect x="2" y="3" width="7.5" height="6" rx="1" />
      <path d="M11 12h4M13 10v4" />
    </svg>
  );
}

export function SpeakerIcon({ size = 14 }: IconProps) {
  return (
    <svg {...SVG_BASE} width={size} height={size}>
      <path d="M2.5 6.2h2.7L9.2 2.8v10.4L5.2 9.8H2.5Z" />
      <path d="M11.4 5.6a3.4 3.4 0 0 1 0 4.8" />
    </svg>
  );
}
