import { useMemo } from "react";

import type { InstalledMod } from "@/lib/tauri";

import { useEffectiveCategories } from "./useEffectiveCategories";

export interface FilterOptions {
  tags: string[];
  champions: string[];
  maps: string[];
}

export function useFilterOptions(mods: InstalledMod[]): FilterOptions {
  const effective = useEffectiveCategories(mods);

  return useMemo(() => {
    const tags = new Set<string>();
    const champions = new Set<string>();
    const maps = new Set<string>();

    for (const mod of mods) {
      const eff = effective.get(mod.id);
      for (const t of eff?.tags ?? mod.tags) tags.add(t);
      for (const c of eff?.champions ?? mod.champions) champions.add(c);
      for (const m of eff?.maps ?? mod.maps) maps.add(m);
    }

    return {
      tags: [...tags].sort(),
      champions: [...champions].sort(),
      maps: [...maps].sort(),
    };
  }, [mods, effective]);
}
