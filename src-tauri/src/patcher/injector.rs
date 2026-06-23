//! External-process injector via the host line protocol.
//!
//! We communicate with the host process over its stdin/stdout line protocol.
//! The host owns all injection logic (window scanning, `SetWindowsHookEx`, DLL
//! pipe) and reports structured lifecycle events back to us.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::host::{self, HostConfig, HostError, HostEvent, HostProcess, HostState, HOST_EXE_NAME};

/// Re-export the executable name that `commands/patcher.rs` resolves.
pub const INJECTOR_EXE_NAME: &str = HOST_EXE_NAME;

#[derive(Debug, thiserror::Error)]
pub enum InjectorError {
    #[error("Host process error: {0}")]
    Host(#[from] HostError),
    #[error("Host injection failed: {0}")]
    Failed(String),
}

/// Spawns and supervises the injection host process.
pub struct Injector {
    exe_path: PathBuf,
    elevate: bool,
}

impl Injector {
    pub fn new(exe_path: PathBuf) -> Self {
        Self {
            exe_path,
            elevate: false,
        }
    }

    /// Enable elevation mode (`--elevate`), which triggers a UAC prompt and
    /// runs the host at high integrity. Required when the game is protected
    /// by Vanguard.
    pub fn with_elevate(mut self, elevate: bool) -> Self {
        self.elevate = elevate;
        self
    }

    /// Run the host-based injector, blocking until the patching session ends
    /// or `stop_flag` is set.
    ///
    /// Sends configuration commands, starts a scan session, then reads event
    /// lines from the host until the game exits or we're told to stop.
    pub fn run(
        &self,
        overlay_dir: &str,
        stop_flag: &AtomicBool,
        config: &HostConfig,
    ) -> Result<(), InjectorError> {
        let mut proc = HostProcess::spawn(&self.exe_path, self.elevate)?;

        // Forward stderr on a background thread for startup diagnostics.
        let stderr_handle = proc.take_stderr().map(forward_stderr);

        // Take the event reader before sending commands so we don't miss events.
        let reader = proc.take_event_reader();

        // Phase 1: Send configuration.
        if let Err(e) = proc.configure(config) {
            tracing::error!("Failed to configure host: {}", e);
            proc.kill();
            join_handle(stderr_handle);
            return Err(e.into());
        }

        // Phase 2: Start scanning.
        if let Err(e) = proc.start_scan() {
            tracing::error!("Failed to start host scan: {}", e);
            proc.kill();
            join_handle(stderr_handle);
            return Err(e.into());
        }

        tracing::info!("Host started, scanning for game (prefix: {})", overlay_dir);

        // Phase 3: Read events until the session ends.
        let result = if let Some(reader) = reader {
            self.event_loop(reader, &mut proc, stop_flag)
        } else {
            tracing::error!("Host stdout not available");
            Err(InjectorError::Host(HostError::StdoutClosed))
        };

        // Cleanup: signal the host to exit and wait for it (with a kill
        // fallback) before joining the stderr forwarder. Joining stderr first
        // would deadlock if the host only exits once its stdin is closed, since
        // stderr won't reach EOF until the host process is gone.
        proc.shutdown();
        join_handle(stderr_handle);

        result
    }

    /// Read and dispatch events from the host until the session is over.
    fn event_loop(
        &self,
        reader: BufReader<std::process::ChildStdout>,
        proc: &mut HostProcess,
        stop_flag: &AtomicBool,
    ) -> Result<(), InjectorError> {
        // `reader.lines()` blocks until a full line arrives or EOF. While the
        // host is silently scanning for the game it emits nothing, so checking
        // the stop flag inline would never fire (the read is parked). Funnel the
        // blocking reads through a channel and poll it with a timeout so the
        // stop flag is honored within the poll interval regardless of host
        // chatter. The reader thread self-terminates on EOF (once the host
        // closes stdout during shutdown) or when the receiver is dropped.
        let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
        thread::spawn(move || {
            for line_result in reader.lines() {
                let is_err = line_result.is_err();
                if tx.send(line_result).is_err() || is_err {
                    break;
                }
            }
        });

        // Tracks the most recent host-reported error so that, if the host then
        // dies, we can surface it to the UI as the failure reason.
        let mut last_error: Option<String> = None;

        loop {
            if stop_flag.load(Ordering::SeqCst) {
                tracing::info!("Stop requested, sending stop to host");
                let _ = proc.stop_session();
                return Ok(());
            }

            let line = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => {
                    tracing::warn!("Host stdout read error: {}", e);
                    if stop_flag.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    return Err(
                        self.unexpected_exit_error(last_error.or_else(|| Some(e.to_string())))
                    );
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // Reader thread ended: the host closed stdout / hit EOF. If we
                    // didn't ask it to stop, the host died on its own — it crashed,
                    // antivirus blocked it, or (on the elevated path) the user
                    // dismissed the UAC prompt. Surface that instead of silently
                    // reporting a clean stop.
                    if stop_flag.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    return Err(self.unexpected_exit_error(last_error));
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            match host::parse_event(&line) {
                Some(HostEvent::Ok { message, .. }) => {
                    tracing::debug!("[cslol-host] ok: {}", message);
                }
                Some(HostEvent::Status { state, message, .. }) => {
                    match state {
                        HostState::Injecting => {
                            tracing::info!("[cslol-host] injecting: {}", message);
                        }
                        HostState::Injected => {
                            tracing::info!("[cslol-host] injected: {}", message);
                        }
                        HostState::Waiting => {
                            tracing::info!("[cslol-host] waiting: {}", message);
                        }
                        HostState::Exited => {
                            tracing::info!("[cslol-host] game exited: {}", message);
                            // Game closed — the host will re-scan automatically
                            // for the next game instance. Keep the event loop
                            // running so injection persists across games until
                            // the user explicitly stops the patcher.
                            tracing::info!("[cslol-host] waiting for next game instance...");
                        }
                        HostState::Failed => {
                            tracing::error!("[cslol-host] failed: {}", message);
                            return Err(InjectorError::Failed(message));
                        }
                    }
                }
                Some(HostEvent::Error { message, .. }) => {
                    // A protocol-level error (e.g. an unrecognized command) is not
                    // necessarily fatal to an in-progress injection — the host
                    // reports fatal injection failures via `status ... failed`. Log
                    // it and keep going; if the host then dies, the EOF branch above
                    // surfaces this message as the failure reason.
                    tracing::warn!("[cslol-host] error: {}", message);
                    last_error = Some(message);
                }
                Some(HostEvent::DllLog {
                    pid, tid, message, ..
                }) => {
                    tracing::info!("[cslol-dll pid={} tid={}] {}", pid, tid, message);
                }
                None => {
                    tracing::trace!("[cslol-host] unparsed: {}", line);
                }
            }
        }
    }

    /// Build the error returned when the host process disappears without us
    /// asking it to stop. Tailors the hint to whether we elevated, since a
    /// dismissed UAC prompt is the most common cause on the elevated path.
    fn unexpected_exit_error(&self, last_error: Option<String>) -> InjectorError {
        let base = if self.elevate {
            "The injection host exited unexpectedly. If you dismissed the Windows User Account Control (UAC) prompt, the patcher cannot run elevated — accept the prompt next time, or turn off \"Run injector elevated\" in Settings if League is not running as administrator."
        } else {
            "The injection host exited unexpectedly. It may have crashed or been blocked by antivirus."
        };
        match last_error {
            Some(detail) if !detail.is_empty() => {
                InjectorError::Failed(format!("{base} (host reported: {detail})"))
            }
            _ => InjectorError::Failed(base.to_string()),
        }
    }
}

/// Forward the host's stderr on a background thread.
fn forward_stderr<R: Read + Send + 'static>(stream: R) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(text) if !text.trim().is_empty() => {
                    tracing::warn!("[cslol-host stderr] {}", text);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

fn join_handle(handle: Option<JoinHandle<()>>) {
    if let Some(h) = handle {
        let _ = h.join();
    }
}
