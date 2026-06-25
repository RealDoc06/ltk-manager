import { PackageX, ShieldAlert } from "lucide-react";
import { useState } from "react";

import { AlertBox, Button, Checkbox, Dialog, Tooltip, useToast } from "@/components";
import type { LinkedBinOffenderInfo, LinkedBinReport, PatcherConfig } from "@/lib/tauri";
import { useInstalledMods, useToggleMod } from "@/modules/library";
import { useLinkedBinGuardStore } from "@/stores";

import { useStartPatcher } from "../api/useStartPatcher";

/** Last path segment of a linked bin path — `data/characters/ahri/ahri.bin` → `ahri.bin`. */
function binBasename(path: string): string {
  const segments = path.split(/[\\/]/);
  return segments[segments.length - 1] ?? path;
}

/**
 * Surfaces the pre-patch linked-bin check (`useGuardedStartPatcher`). When an enabled
 * mod ships a property-bin whose linked dependencies won't resolve at load time, the
 * game can crash or misbehave. This dialog lets the user disable the offending mod(s)
 * before patching or start anyway and accept the risk. Mounted globally so it works
 * for both the manual and auto-start paths.
 */
export function LinkedBinWarningDialog() {
  const pending = useLinkedBinGuardStore((s) => s.pending);
  const clear = useLinkedBinGuardStore((s) => s.clear);

  // Render the content (and its mutations) only while a warning is pending.
  if (!pending) return null;

  return (
    <LinkedBinWarningContent report={pending.report} config={pending.config} onClose={clear} />
  );
}

function LinkedBinWarningContent({
  report,
  config,
  onClose,
}: {
  report: LinkedBinReport;
  config: PatcherConfig;
  onClose: () => void;
}) {
  const { data: mods = [] } = useInstalledMods();
  const toggleMod = useToggleMod();
  const startPatcher = useStartPatcher();
  const toast = useToast();

  const { offenders } = report;
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(offenders.map((o) => o.modId)),
  );
  const [busy, setBusy] = useState(false);

  const selectedCount = selected.size;
  const isMulti = offenders.length > 1;
  const allSelected = selectedCount === offenders.length;

  const displayNameFor = (offender: LinkedBinOffenderInfo) =>
    mods.find((m) => m.id === offender.modId)?.displayName ?? offender.displayName;

  const toggleSelected = (modId: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(modId)) {
        next.delete(modId);
      } else {
        next.add(modId);
      }
      return next;
    });

  const toggleAll = () =>
    setSelected(() => (allSelected ? new Set() : new Set(offenders.map((o) => o.modId))));

  const startWith = (configToUse: PatcherConfig) => {
    startPatcher.mutate(configToUse);
    onClose();
  };

  const handleDisableAndStart = async () => {
    setBusy(true);
    try {
      for (const offender of offenders) {
        if (selected.has(offender.modId)) {
          await toggleMod.mutateAsync({ modId: offender.modId, enabled: false });
        }
      }
      toast.warning(
        selectedCount === 1 ? "Mod disabled" : "Mods disabled",
        `${selectedCount} mod${selectedCount === 1 ? "" : "s"} with missing dependencies won't be loaded`,
      );
      startWith(config);
    } catch {
      setBusy(false);
      toast.error("Couldn't disable mods", "Try again, or start anyway");
    }
  };

  const title = isMulti ? "Some mods are missing dependencies" : "A mod is missing dependencies";
  const sectionLabel = isMulti ? `Offending mods (${offenders.length})` : "Offending mod";
  const primaryLabel = selectedCount <= 1 ? "Disable & start" : `Disable ${selectedCount} & start`;

  return (
    <Dialog.Root open onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Backdrop />
        <Dialog.Overlay size="lg">
          <Dialog.Header>
            <Dialog.Title className="flex items-center gap-2.5">
              <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-amber-500/15 text-amber-400">
                <PackageX className="h-4 w-4" />
              </span>
              {title}
            </Dialog.Title>
            <Dialog.Close className="text-amber-400 hover:bg-amber-500/15 hover:text-amber-300" />
          </Dialog.Header>

          <Dialog.Body className="flex flex-col gap-5">
            <p className="text-sm leading-relaxed text-surface-300">
              These enabled mods contain references to data game files that aren&apos;t installed.
              You may experience weird glitches or crashes when League loads them.
            </p>

            <div className="flex flex-col gap-2.5">
              <div className="flex items-center justify-between">
                <span className="text-xs font-medium tracking-wide text-surface-400 uppercase">
                  {sectionLabel}
                </span>
                {isMulti && (
                  <Button
                    variant="transparent"
                    size="xs"
                    compact
                    className="text-accent-400 hover:text-accent-300"
                    onClick={toggleAll}
                  >
                    {allSelected ? "Deselect all" : "Select all"}
                  </Button>
                )}
              </div>

              <div className="flex max-h-72 flex-col gap-2 overflow-y-auto pr-1">
                {offenders.map((offender) => {
                  const checked = selected.has(offender.modId);
                  return (
                    <div
                      key={offender.modId}
                      className="overflow-hidden rounded-lg border border-surface-700 bg-surface-900/60"
                    >
                      <label className="flex cursor-pointer items-center gap-3 px-3 py-2.5">
                        <Checkbox
                          size="sm"
                          checked={checked}
                          onCheckedChange={() => toggleSelected(offender.modId)}
                        />
                        <span className="min-w-0 flex-1 truncate text-sm font-medium text-surface-100">
                          {displayNameFor(offender)}
                        </span>
                        <span className="shrink-0 rounded-full bg-amber-500/10 px-2 py-0.5 text-xs font-medium text-amber-300">
                          {offender.missingLinks.length} missing
                        </span>
                      </label>
                      <ul className="flex flex-col gap-1 border-t border-surface-800 bg-surface-950/40 px-3 py-2">
                        {offender.missingLinks.map((link) => (
                          <Tooltip
                            key={link}
                            side="top"
                            align="start"
                            content={
                              <span className="block max-w-sm font-mono text-xs break-all">
                                {link}
                              </span>
                            }
                          >
                            <li className="truncate font-mono text-xs text-surface-400">
                              {binBasename(link)}
                            </li>
                          </Tooltip>
                        ))}
                      </ul>
                    </div>
                  );
                })}
              </div>
            </div>

            <AlertBox
              variant="warning"
              icon={<ShieldAlert className="h-5 w-5" />}
              title="Keeping the flagged mods as enabled risks issues and crashes we can't control."
            />
          </Dialog.Body>

          <Dialog.Footer className="justify-between">
            <Button variant="ghost" className="text-surface-400" disabled={busy} onClick={onClose}>
              Cancel
            </Button>
            <div className="flex items-center gap-2">
              <Button variant="ghost" disabled={busy} onClick={() => startWith(config)}>
                Start anyway
              </Button>
              <Button
                variant="filled"
                loading={busy}
                disabled={selectedCount === 0}
                onClick={handleDisableAndStart}
              >
                {primaryLabel}
              </Button>
            </div>
          </Dialog.Footer>
        </Dialog.Overlay>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
