use super::{
    BackendError, BackendResult, PatcherAvailability, PatcherBackend, PatcherContext,
    PatcherEventSink, PatcherPreflight,
};
use crate::error::{AppError, AppResult};
use crate::legacy_patcher::api::PATCHER_DLL_NAME;
use crate::legacy_patcher::runner::{run_legacy_patcher_loop, LegacyPatcherLoopError};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub struct WindowsDllBackend {
    app_handle: AppHandle,
}

impl WindowsDllBackend {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    fn resolve_dll(&self) -> AppResult<PathBuf> {
        let resource_path = self
            .app_handle
            .path()
            .resource_dir()
            .map_err(|error| AppError::Other(format!("Failed to get resource directory: {error}")))?
            .join(PATCHER_DLL_NAME);
        if resource_path.exists() {
            return Ok(resource_path);
        }

        if let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join(PATCHER_DLL_NAME)))
            .filter(|path| path.exists())
        {
            return Ok(path);
        }

        let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(PATCHER_DLL_NAME);
        if manifest_path.exists() {
            return Ok(manifest_path);
        }

        Err(AppError::Other(format!(
            "Patcher DLL not found at {}",
            manifest_path.display()
        )))
    }
}

impl PatcherBackend for WindowsDllBackend {
    fn name(&self) -> &'static str {
        "windows-dll"
    }

    fn availability(&self) -> PatcherAvailability {
        match self.resolve_dll() {
            Ok(_) => PatcherAvailability {
                supported: true,
                ready: true,
                reason: None,
                requires_setup: false,
                permission_required: false,
                helper_version: None,
            },
            Err(error) => PatcherAvailability {
                supported: true,
                ready: false,
                reason: Some(error.to_string()),
                requires_setup: true,
                permission_required: false,
                helper_version: None,
            },
        }
    }

    fn preflight(&self, _context: &PatcherContext) -> AppResult<PatcherPreflight> {
        self.resolve_dll()?;
        Ok(PatcherPreflight {
            compatible: true,
            backend: self.name().into(),
            architecture: std::env::consts::ARCH.into(),
            signature: None,
            reason: None,
        })
    }

    fn run(
        &self,
        context: PatcherContext,
        stop: Arc<AtomicBool>,
        events: PatcherEventSink,
    ) -> BackendResult<()> {
        let dll = self.resolve_dll().map_err(|error| BackendError::Failed {
            code: "PATCHER_DLL_MISSING".into(),
            detail: error.to_string(),
        })?;
        let mut overlay = context.overlay_root.display().to_string();
        if !overlay.ends_with(std::path::MAIN_SEPARATOR) {
            overlay.push(std::path::MAIN_SEPARATOR);
        }
        events(super::BackendEvent {
            event: "waitingForGame".into(),
            pid: None,
            architecture: Some(std::env::consts::ARCH.into()),
            signature: None,
            detail: None,
        });
        match run_legacy_patcher_loop(
            &dll,
            &overlay,
            context.log_file.as_deref(),
            context.timeout_ms,
            context.flags,
            &stop,
        ) {
            Ok(()) => Ok(()),
            Err(LegacyPatcherLoopError::Stopped) => Err(BackendError::Stopped),
            Err(error) => Err(BackendError::Failed {
                code: "WINDOWS_PATCHER_FAILED".into(),
                detail: error.to_string(),
            }),
        }
    }
}
