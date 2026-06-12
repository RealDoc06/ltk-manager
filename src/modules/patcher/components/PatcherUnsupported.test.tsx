import { render, screen } from "@testing-library/react";

import type { PlatformSupport } from "@/lib/tauri";

import { PatcherUnsupported } from "./PatcherUnsupported";

function platform(patcher: Partial<PlatformSupport["patcher"]>): PlatformSupport {
  return {
    os: "macos",
    architecture: "aarch64",
    patcher: {
      supported: true,
      ready: false,
      reason: null,
      requiresSetup: false,
      permissionRequired: true,
      helperVersion: null,
      ...patcher,
    },
    hotkeys: {
      supported: true,
      accessibilityPermissionRequired: false,
      reason: null,
    },
  };
}

describe("PatcherUnsupported", () => {
  it("shows helper setup state separately from unsupported platforms", () => {
    render(
      <PatcherUnsupported
        platform={platform({
          requiresSetup: true,
          reason: "Run pnpm macos:helper",
        })}
      />,
    );

    expect(screen.getByText("macOS patcher helper is not ready")).toBeInTheDocument();
    expect(screen.getByText("Run pnpm macos:helper")).toBeInTheDocument();
  });

  it("shows unsupported platform state", () => {
    render(
      <PatcherUnsupported
        platform={platform({
          supported: false,
          permissionRequired: false,
          reason: "Unsupported operating system",
        })}
      />,
    );

    expect(screen.getByText("Patcher not available on this platform")).toBeInTheDocument();
  });
});
