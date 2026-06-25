import { create } from "zustand";

import type { LinkedBinReport, PatcherConfig } from "@/lib/tauri";

interface PendingLinkedBinWarning {
  report: LinkedBinReport;
  /** The patcher config to start with once the user resolves the warning. */
  config: PatcherConfig;
}

interface LinkedBinGuardStore {
  /** Set while a pre-patch linked-bin warning is awaiting the user's decision. */
  pending: PendingLinkedBinWarning | null;
  setPending: (pending: PendingLinkedBinWarning) => void;
  clear: () => void;
}

/**
 * Holds the pending linked-bin warning surfaced by the pre-patch check, so the
 * globally-mounted `LinkedBinWarningDialog` can render it regardless of which start
 * path (manual or auto-start) triggered the check.
 */
export const useLinkedBinGuardStore = create<LinkedBinGuardStore>((set) => ({
  pending: null,
  setPending: (pending) => set({ pending }),
  clear: () => set({ pending: null }),
}));
