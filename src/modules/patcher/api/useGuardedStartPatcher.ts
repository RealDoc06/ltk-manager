import { useCallback } from "react";

import { useToast } from "@/components";
import { api, type PatcherConfig } from "@/lib/tauri";
import { checkModForSkinhack, useInstalledMods } from "@/modules/library";
import { useLinkedBinGuardStore } from "@/stores";

import { useCheckLinkedBins } from "./useCheckLinkedBins";
import { useStartPatcher } from "./useStartPatcher";

/**
 * Shared pre-patch gate for both the manual and auto-start paths. In order it:
 * force-disables any enabled skinhack mods (which would crash the game), runs the
 * linked-bin check and defers to the globally-mounted `LinkedBinWarningDialog` when
 * a mod has unresolved dependencies, then starts the patcher. A failed linked-bin
 * check never blocks the patch, but a start failure surfaces a toast.
 */
export function useGuardedStartPatcher() {
  const startPatcher = useStartPatcher();
  const checkLinkedBins = useCheckLinkedBins();
  const setPending = useLinkedBinGuardStore((s) => s.setPending);
  const { data: mods = [] } = useInstalledMods();
  const toast = useToast();

  return useCallback(
    async (config: PatcherConfig = {}) => {
      const enabledMods = mods.filter((m) => m.enabled);
      const flaggedMods = enabledMods.filter((m) => checkModForSkinhack(m) != null);
      for (const mod of flaggedMods) {
        await api.toggleMod(mod.id, false);
        toast.warning(
          "Skinhack Excluded",
          `"${mod.displayName}" was detected as a skinhack and won't be loaded`,
        );
      }

      // Nothing safe left to apply once every enabled mod was a flagged skinhack.
      if (flaggedMods.length >= enabledMods.length) {
        return;
      }

      try {
        const report = await checkLinkedBins.mutateAsync();
        if (report.offenders.length > 0) {
          setPending({ report, config });
          return;
        }
      } catch (error) {
        console.error("Linked-bin pre-flight check failed:", error);
      }

      try {
        await startPatcher.mutateAsync(config);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        toast.error("Couldn't start the patcher", message);
        console.error("Failed to start patcher:", error);
      }
    },
    [mods, checkLinkedBins, setPending, startPatcher, toast],
  );
}
