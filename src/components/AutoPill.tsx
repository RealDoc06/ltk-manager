import { Sparkles } from "lucide-react";
import { twMerge } from "tailwind-merge";

type AutoPillTone = "accent" | "emerald" | "sky";

const TONE_CLASSES: Record<AutoPillTone, string> = {
  accent: "border-accent-400/60 bg-accent-500/10 text-accent-300",
  emerald: "border-emerald-400/60 bg-emerald-500/10 text-emerald-300",
  sky: "border-sky-400/60 bg-sky-500/10 text-sky-300",
};

interface AutoPillProps {
  label: string;
  tone?: AutoPillTone;
  /** When provided, the pill renders as a button (an actionable suggestion). */
  onClick?: () => void;
  className?: string;
}

/**
 * A dashed-outline pill marking an auto-detected (WAD-footprint-derived)
 * category. Static for display; pass `onClick` to use it as a clickable
 * suggestion chip.
 */
export function AutoPill({ label, tone = "accent", onClick, className }: AutoPillProps) {
  const classes = twMerge(
    "inline-flex items-center gap-0.5 rounded border border-dashed px-1.5 py-0.5 text-[10px] leading-tight",
    TONE_CLASSES[tone],
    onClick && "cursor-pointer transition-colors hover:bg-surface-700/40",
    className,
  );

  if (onClick) {
    return (
      <button type="button" onClick={onClick} className={classes}>
        <Sparkles className="h-2.5 w-2.5" />
        {label}
      </button>
    );
  }

  return (
    <span className={classes}>
      <Sparkles className="h-2.5 w-2.5" />
      {label}
    </span>
  );
}
