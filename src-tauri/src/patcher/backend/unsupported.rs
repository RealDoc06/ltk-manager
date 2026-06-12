use super::{
    BackendError, BackendResult, PatcherAvailability, PatcherBackend, PatcherContext,
    PatcherEventSink, PatcherPreflight,
};
use crate::error::{AppError, AppResult};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub struct UnsupportedBackend;

impl PatcherBackend for UnsupportedBackend {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn availability(&self) -> PatcherAvailability {
        PatcherAvailability::unsupported("Live patching is not supported on this operating system")
    }

    fn preflight(&self, _context: &PatcherContext) -> AppResult<PatcherPreflight> {
        Err(AppError::Other(
            "Live patching is not supported on this operating system".into(),
        ))
    }

    fn run(
        &self,
        _context: PatcherContext,
        _stop: Arc<AtomicBool>,
        _events: PatcherEventSink,
    ) -> BackendResult<()> {
        Err(BackendError::Failed {
            code: "UNSUPPORTED_PLATFORM".into(),
            detail: "Live patching is not supported on this operating system".into(),
        })
    }
}
