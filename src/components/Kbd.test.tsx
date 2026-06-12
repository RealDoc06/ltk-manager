import { render, screen } from "@testing-library/react";

import { Kbd } from "./Kbd";

describe("Kbd", () => {
  const platform = Object.getOwnPropertyDescriptor(navigator, "platform");

  afterEach(() => {
    if (platform) {
      Object.defineProperty(navigator, "platform", platform);
    } else {
      Reflect.deleteProperty(navigator, "platform");
    }
  });

  it("uses macOS modifier labels for portable shortcuts", () => {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "MacIntel",
    });
    render(<Kbd shortcut="CommandOrControl+Alt+Shift+P" />);

    expect(screen.getByText("⌘")).toBeInTheDocument();
    expect(screen.getByText("⌥")).toBeInTheDocument();
    expect(screen.getByText("⇧")).toBeInTheDocument();
  });

  it("uses Ctrl outside macOS", () => {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "Win32",
    });
    render(<Kbd shortcut="CommandOrControl+P" />);

    expect(screen.getByText("Ctrl")).toBeInTheDocument();
  });
});
