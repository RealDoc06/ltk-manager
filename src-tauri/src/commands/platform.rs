use crate::error::IpcResult;
use crate::patcher::backend::{selected_backend, PatcherAvailability};
use serde::Serialize;
use tauri::AppHandle;
use ts_rs::TS;

#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyAvailability {
    pub supported: bool,
    pub accessibility_permission_required: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSupport {
    pub os: String,
    pub architecture: String,
    pub patcher: PatcherAvailability,
    pub hotkeys: HotkeyAvailability,
}

#[tauri::command]
pub fn get_platform_support(app_handle: AppHandle) -> IpcResult<PlatformSupport> {
    let patcher = selected_backend(&app_handle).availability();
    let hotkeys_supported = cfg!(any(target_os = "windows", target_os = "macos"));
    IpcResult::ok(PlatformSupport {
        os: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        patcher,
        hotkeys: HotkeyAvailability {
            supported: hotkeys_supported,
            accessibility_permission_required: false,
            reason: (!hotkeys_supported)
                .then(|| "Global shortcuts are not supported on this operating system".into()),
        },
    })
}
