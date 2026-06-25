# Development Guide

This guide covers how to build LTK Manager from source and contribute to the project.

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) 22+
- [pnpm](https://pnpm.io/)
- Platform-specific dependencies (see below)

### Linux

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

### Windows

No additional system dependencies required beyond Rust and Node.js.

### macOS

- macOS 13 or newer
- Apple Silicon (`arm64`)
- Xcode Command Line Tools (`xcode-select --install`)
- SIP enabled

The initial native patcher does not support Intel Macs or an x86_64 League process under Rosetta.

## Getting Started

```bash
git clone https://github.com/LeagueToolkit/ltk-manager.git
cd ltk-manager
pnpm install
```

### Development Mode

```bash
# Full dev mode — Rust backend + React frontend with hot reload
pnpm tauri dev

# Frontend only — skips the Rust rebuild, faster for UI iteration
pnpm dev
```

## Apple Silicon Development

The macOS patcher is a separate executable. LTK Manager stays unprivileged; starting a patcher
session launches only the small helper through a macOS administrator approval prompt. The helper
connects back to an owner-only Unix socket, authenticates with a random session token, validates
the configured League bundle and overlay paths, and exits when the patcher stops.

### Build and run

```bash
pnpm install
pnpm macos:dev
```

`pnpm macos:dev` builds and ad-hoc signs
`src-tauri/binaries/ltk-macos-patcher-aarch64-apple-darwin`, then starts
`pnpm tauri dev --target aarch64-apple-darwin`.

For a local ARM64 application bundle:

```bash
pnpm macos:build
```

This produces `target/aarch64-apple-darwin/release/bundle/macos/LTK Manager.app`.
The build command ad-hoc signs the helper and final app bundle, then runs strict local signature
verification.
Developer ID signing, notarization, DMG packaging, updater artifacts, Intel builds, and universal
binaries are intentionally outside the local workflow.

### Configure League

LTK Manager accepts any of these selections and resolves them to one canonical installation:

- `/Applications/League of Legends.app`
- `League of Legends.app/Contents/LoL`
- `League of Legends.app/Contents/LoL/Game`
- A path inside `Game/LeagueofLegends.app`

Auto-detection checks common application locations and running `LeagueClient` or
`LeagueofLegends` process paths. `LTK_LEAGUE_PATH` can be set to override detection during local
development.

### Verify compatibility

Run a read-only dry scan before starting the patcher:

```bash
pnpm macos:preflight -- "/Applications/League of Legends.app"
```

A successful response reports `compatible`, architecture `arm64`, and the current signature ID.
The helper requires exactly one validated patch signature and fails before opening or writing
target process memory when the League build is unknown.

To inspect the installed executable directly:

```bash
file "/Applications/League of Legends.app/Contents/LoL/Game/LeagueofLegends.app/Contents/MacOS/LeagueofLegends"
```

The selected slice must be ARM64. Rosetta/x86_64 execution is not supported.

### End-to-end run

1. Keep SIP enabled.
2. Start LTK Manager with `pnpm macos:dev`.
3. Select or auto-detect the League application.
4. Install and enable a harmless, visually obvious test mod.
5. Run Diagnostics and confirm the native helper and ARM64 signature checks pass.
6. Click Run and approve the macOS administrator prompt for the helper.
7. Launch Practice Tool and verify the mod loads from the generated overlay.
8. Stop the patcher, launch another Practice Tool session, and verify unmodified assets are used.

Repeat the patched and unpatched cycle after every League update. A signature mismatch is a hard
compatibility failure and must not be bypassed.

### Stop, repair, and remove

The privilege mechanism is one-shot: no launch daemon or persistent root helper is installed.
Stopping the patcher sends a protocol stop request and waits for the elevated helper to exit.

Rebuild a missing or version-mismatched helper:

```bash
pnpm macos:helper
```

Remove local helper artifacts:

```bash
rm -f src-tauri/binaries/ltk-macos-patcher-aarch64-apple-darwin
cargo clean -p ltk-macos-patcher
```

If macOS App Translocation or quarantine prevents helper startup, move the locally built app to
`/Applications`, verify its source, and use the Diagnostics report before changing any extended
attributes. Do not disable SIP and do not run the full Tauri application with `sudo`.

### Patch maintenance

The patch-day smoke sequence is:

1. Rebuild the helper.
2. Run `pnpm macos:preflight`.
3. Build an overlay from the fixed harmless test mod.
4. Verify one patched Practice Tool launch.
5. Stop patching and verify one unmodified launch.
6. Repeat after restarting LTK Manager.

Update the versioned native signature only with an ARM64 executable fixture and a unique validated
match. The helper must continue to reject zero or multiple matches.

### Verbose Backend Logging

```bash
RUST_LOG=ltk_manager=trace,tauri=info pnpm tauri dev
```

## Code Quality

```bash
pnpm typecheck        # TypeScript type checking
pnpm lint             # ESLint
pnpm format           # Prettier (auto-fix)
pnpm check            # All three (typecheck + lint + format:check)

# Rust
cargo clippy --workspace --all-targets
cargo fmt --all
```

## Production Build

```bash
pnpm tauri build
```

Output is written to `src-tauri/target/release/bundle/`:

| Platform | Path                                     | Format             |
| -------- | ---------------------------------------- | ------------------ |
| Windows  | `bundle/nsis/LTK Manager_*-setup.exe`    | NSIS installer     |
| Windows  | `bundle/msi/LTK Manager_*.msi`           | MSI installer      |
| macOS    | `bundle/dmg/LTK Manager_*.dmg`           | DMG disk image     |
| macOS    | `bundle/macos/LTK Manager.app`           | Application bundle |
| Linux    | `bundle/deb/ltk-manager_*.deb`           | Debian package     |
| Linux    | `bundle/appimage/ltk-manager_*.AppImage` | AppImage           |

To build a specific format:

```bash
pnpm tauri build --bundles nsis   # Windows NSIS only
pnpm tauri build --bundles msi    # Windows MSI only
pnpm tauri build --bundles dmg    # macOS DMG only
pnpm tauri build --bundles deb    # Linux Debian only
```

For faster unoptimized builds during development:

```bash
pnpm tauri build --debug
```

## Project Structure

```
ltk-manager/
├── src/                        # React frontend
│   ├── components/             # Shared UI components (wrapping base-ui)
│   ├── modules/                # Feature modules
│   │   ├── library/            # Mod library management
│   │   ├── patcher/            # Overlay patcher
│   │   ├── settings/           # App settings and theming
│   │   └── workshop/           # Creator tools
│   ├── routes/                 # File-based routing (TanStack Router)
│   ├── lib/                    # Tauri bindings and utilities
│   ├── stores/                 # Zustand client-side stores
│   └── styles/                 # Tailwind CSS + theme variables
├── src-tauri/                  # Rust backend (Tauri v2)
│   └── src/
│       ├── main.rs             # App setup, command registration
│       ├── commands/           # IPC command handlers
│       ├── mods/               # Mod install/uninstall/toggle logic
│       ├── overlay/            # Overlay building
│       ├── patcher/            # Patcher lifecycle + external injection host (cslol-host.exe)
│       ├── state.rs            # App state and settings
│       └── error.rs            # Error types and IPC result helpers
├── docs/                       # Documentation
├── .github/workflows/          # CI and release pipelines
├── package.json
└── vite.config.ts
```

## Tech Stack

| Layer             | Technology                             |
| ----------------- | -------------------------------------- |
| Desktop framework | Tauri v2                               |
| Backend           | Rust                                   |
| Frontend          | React 19, TypeScript, Vite             |
| Styling           | Tailwind CSS v4                        |
| Routing           | TanStack Router (file-based)           |
| Server state      | TanStack Query                         |
| Client state      | Zustand                                |
| Forms             | TanStack Form + Zod                    |
| UI primitives     | base-ui (wrapped in `src/components/`) |

## Log Files

Logs are written to disk automatically and are useful for debugging:

- **Windows:** `%APPDATA%\dev.leaguetoolkit.manager\logs\ltk-manager.log`
- **Linux:** `~/.local/share/dev.leaguetoolkit.manager/logs/ltk-manager.log`
- **macOS:** `~/Library/Logs/dev.leaguetoolkit.manager/ltk-manager.*.log`
