import { useMutation } from "@tanstack/react-query";

import { api, type AppError, type LinkedBinReport } from "@/lib/tauri";
import { unwrapForQuery } from "@/utils/query";

/**
 * Runs the backend pre-patch linked-bin check, which scans enabled library mods for
 * property-bins whose linked dependencies won't resolve at load time.
 */
export function useCheckLinkedBins() {
  return useMutation<LinkedBinReport, AppError, void>({
    mutationFn: async () => {
      const result = await api.checkLinkedBins();
      return unwrapForQuery(result);
    },
  });
}
