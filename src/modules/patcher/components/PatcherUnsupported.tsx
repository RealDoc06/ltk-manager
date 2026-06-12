import { Monitor, ShieldAlert, Wrench } from "lucide-react";

import type { PlatformSupport } from "@/lib/tauri";

export function PatcherUnsupported({ platform }: { platform?: PlatformSupport }) {
  const patcher = platform?.patcher;
  const Icon = patcher?.requiresSetup
    ? Wrench
    : patcher?.permissionRequired
      ? ShieldAlert
      : Monitor;
  const title = !patcher?.supported
    ? "Patcher not available on this platform"
    : patcher.requiresSetup
      ? "macOS patcher helper is not ready"
      : "Patcher setup is required";
  const detail =
    patcher?.reason ??
    "Mod management works normally, but this platform cannot run the live overlay patcher.";

  return (
    <div className="flex items-center gap-3 rounded-lg border border-surface-600 bg-surface-800/50 px-4 py-3">
      <Icon className="h-5 w-5 shrink-0 text-surface-400" />
      <div className="flex flex-col">
        <span className="text-sm font-medium text-surface-200">{title}</span>
        <span className="text-xs text-surface-400">{detail}</span>
      </div>
    </div>
  );
}
