import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect } from "react";

import { useLinkedBinGuardStore } from "@/stores";

/**
 * Surfaces and focuses the main window whenever a linked-bin warning is pending.
 *
 * The warning blocks the patcher until the user decides, so it must not stay
 * hidden when auto-start fires in tray mode. No-op when the window is already
 * visible (the manual start path).
 */
export function useSurfaceLinkedBinWarning() {
  const pending = useLinkedBinGuardStore((s) => s.pending);

  useEffect(() => {
    if (!pending) return;
    const win = getCurrentWindow();
    void win.show();
    void win.setFocus();
  }, [pending]);
}
