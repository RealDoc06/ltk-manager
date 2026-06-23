import { useMemo } from "react";

/**
 * Filter `availableWads` to every suggestion that contains the trimmed `draft`
 * (case-insensitive) and isn't in `excluded`, preserving input order.
 *
 * Returns an empty list when `availableWads` is empty. An empty `draft` yields
 * all non-excluded WADs. The result is unbounded — the consuming dropdown
 * virtualizes, so rendering the full set stays cheap.
 */
export function useWadAutocomplete(
  draft: string,
  availableWads: string[],
  excluded: Set<string>,
): string[] {
  return useMemo(() => {
    const q = draft.trim().toLowerCase();
    const pool = availableWads.filter((w) => !excluded.has(w));
    if (!q) return pool;
    return pool.filter((w) => w.includes(q));
  }, [draft, availableWads, excluded]);
}
