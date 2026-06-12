# Native patcher notice

The ARM64 Mach-O scanner and in-process WAD redirection mechanism are adapted
from the `cslol-tools` subtree of LeagueToolkit/cslol-manager, commit
`23f230858bc2359ce279e07ed129d482fe3b00bf`. That subtree carries an MIT
license. The protocol, validation, lifecycle, and privilege boundary in this
directory are specific to LTK Manager.

This helper is intentionally a separate process. It must not be linked into or
loaded by the Tauri WebView process.
