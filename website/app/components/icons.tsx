type IconProps = { className?: string };

export function GitHubIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className}>
      <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.57.1.78-.25.78-.55 0-.27-.01-1.17-.02-2.12-3.2.7-3.88-1.35-3.88-1.35-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.02 1.75 2.68 1.25 3.34.95.1-.74.4-1.25.72-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.26.45-2.28 1.18-3.09-.12-.29-.51-1.46.11-3.04 0 0 .96-.31 3.15 1.18a10.9 10.9 0 015.73 0c2.19-1.49 3.15-1.18 3.15-1.18.62 1.58.23 2.75.11 3.04.74.81 1.18 1.83 1.18 3.09 0 4.41-2.69 5.39-5.25 5.67.41.36.78 1.06.78 2.14 0 1.54-.01 2.79-.01 3.17 0 .3.21.66.79.55A10.51 10.51 0 0023.5 12C23.5 5.65 18.35.5 12 .5z" />
    </svg>
  );
}

export function CheckIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 20 20" fill="none" className={className}>
      <path
        d="M4 10.5l3.5 3.5L16 5.5"
        stroke="var(--success)"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function CrossIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 20 20" fill="none" className={className}>
      <path
        d="M5 5l10 10M15 5L5 15"
        stroke="var(--dim)"
        strokeWidth="2"
        strokeLinecap="round"
      />
    </svg>
  );
}

export function DashIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 20 20" fill="none" className={className}>
      <path
        d="M5 10h10"
        stroke="var(--accent)"
        strokeWidth="2"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** 通用抽象图标集，用于「集成」徽标区——不复刻任何品牌官方 logo */
export function RustMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <circle cx="12" cy="12" r="8.5" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="12" cy="12" r="2.5" fill="currentColor" />
      {Array.from({ length: 8 }).map((_, i) => {
        const angle = (i * Math.PI) / 4;
        const x1 = 12 + Math.cos(angle) * 8.5;
        const y1 = 12 + Math.sin(angle) * 8.5;
        const x2 = 12 + Math.cos(angle) * 10.2;
        const y2 = 12 + Math.sin(angle) * 10.2;
        return (
          <line
            key={i}
            x1={x1}
            y1={y1}
            x2={x2}
            y2={y2}
            stroke="currentColor"
            strokeWidth="1.4"
          />
        );
      })}
    </svg>
  );
}

export function GpuMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <rect x="3.5" y="3.5" width="17" height="17" rx="2" stroke="currentColor" strokeWidth="1.4" />
      <rect x="7.5" y="7.5" width="9" height="9" rx="1" stroke="currentColor" strokeWidth="1.4" />
    </svg>
  );
}

export function SparkMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className}>
      <path d="M12 2l1.8 6.2L20 10l-6.2 1.8L12 18l-1.8-6.2L4 10l6.2-1.8L12 2z" />
    </svg>
  );
}

export function GitMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <circle cx="7" cy="6" r="2" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="7" cy="18" r="2" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="17" cy="12" r="2" stroke="currentColor" strokeWidth="1.4" />
      <path d="M7 8v8M7 8c0 4 4 4 8 4" stroke="currentColor" strokeWidth="1.4" />
    </svg>
  );
}

export function LayersMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <path
        d="M12 3l8 4-8 4-8-4 8-4zM4 12l8 4 8-4M4 16l8 4 8-4"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function SearchMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <circle cx="10.5" cy="10.5" r="6" stroke="currentColor" strokeWidth="1.4" />
      <path d="M15.5 15.5L20 20" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}

export function AppleMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <rect x="4" y="6" width="16" height="11" rx="1.5" stroke="currentColor" strokeWidth="1.4" />
      <path d="M9 20h6M12 17v3" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}

export function TermMark({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className}>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" stroke="currentColor" strokeWidth="1.4" />
      <path d="M7 9l3 3-3 3M12 15h5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function AppleLogo({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path d="M16.37 12.73c.03 2.72 2.39 3.62 2.42 3.64-.02.07-.38 1.3-1.25 2.57-.75 1.1-1.53 2.19-2.76 2.21-1.2.03-1.59-.71-2.96-.71-1.38 0-1.81.69-2.94.73-1.19.04-2.09-1.19-2.85-2.28-1.55-2.24-2.73-6.33-1.14-9.09.78-1.37 2.19-2.24 3.72-2.26 1.16-.02 2.26.78 2.97.78.7 0 2.02-.97 3.41-.82.58.02 2.21.23 3.26 1.77-.08.05-1.94 1.13-1.92 3.46zM14.7 6.3c.63-.76 1.05-1.82.94-2.87-.91.04-2 .6-2.65 1.36-.58.67-1.09 1.74-.95 2.77 1.01.08 2.04-.51 2.66-1.26z" />
    </svg>
  );
}

export function DesktopIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className} aria-hidden="true">
      <rect x="3" y="4" width="18" height="12" rx="1.6" stroke="currentColor" strokeWidth="1.6" />
      <path d="M8 20h8M12 16v4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

export function PhoneIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className} aria-hidden="true">
      <rect x="7" y="2.75" width="10" height="18.5" rx="2.4" stroke="currentColor" strokeWidth="1.6" />
      <path d="M11 18.25h2" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

export function BookIcon({ className }: IconProps) {
  return (
    <svg viewBox="0 0 24 24" fill="none" className={className} aria-hidden="true">
      <path
        d="M5 4.75h9.2A3.3 3.3 0 0 1 17.5 8v11.25H8.3A3.3 3.3 0 0 0 5 16.05V4.75Z"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
      <path d="M5 16.05A3.3 3.3 0 0 1 8.3 12.8H17.5" stroke="currentColor" strokeWidth="1.6" />
    </svg>
  );
}
