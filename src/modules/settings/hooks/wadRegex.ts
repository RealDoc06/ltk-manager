/**
 * Pure helpers for working with user-entered regex patterns against the WAD
 * list. Separated from the hooks so they can be unit-tested without React.
 */

/** Return `true` if `pattern` compiles as a JS regex (case-insensitive). */
export function isValidRegex(pattern: string): boolean {
  try {
    new RegExp(pattern, "i");
    return true;
  } catch {
    return false;
  }
}

/**
 * Count how many of `wads` match `pattern`. Returns `0` for invalid patterns
 * so callers don't have to guard — the "is it valid" question is separate.
 */
export function countRegexMatches(pattern: string, wads: string[]): number {
  try {
    const re = new RegExp(pattern, "i");
    let n = 0;
    for (const wad of wads) {
      if (re.test(wad)) n++;
    }
    return n;
  } catch {
    return 0;
  }
}

/**
 * Return the WADs in `wads` that match `pattern`, preserving input order.
 * Returns `[]` for invalid patterns so callers don't have to guard.
 */
export function listRegexMatches(pattern: string, wads: string[]): string[] {
  try {
    const re = new RegExp(pattern, "i");
    return wads.filter((wad) => re.test(wad));
  } catch {
    return [];
  }
}
