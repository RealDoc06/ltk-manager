use super::{
    BackendError, BackendEvent, BackendResult, PatcherAvailability, PatcherBackend, PatcherContext,
    PatcherEventSink, PatcherPreflight,
};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub const MACOS_HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELPER_NAME: &str = "ltk-macos-patcher";
const HELPER_TARGET_NAME: &str = "ltk-macos-patcher-aarch64-apple-darwin";
const PROTOCOL_VERSION: u32 = 1;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const IO_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperEvent {
    version: u32,
    event: String,
    code: Option<String>,
    detail: Option<String>,
    signature: Option<String>,
    architecture: Option<String>,
    pid: Option<u32>,
    helper_version: Option<String>,
    token: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest<'a> {
    version: u32,
    command: &'static str,
    token: &'a str,
    overlay: &'a Path,
    allowed_root: &'a Path,
    game_bundle: &'a Path,
    game_executable: &'a Path,
    client_uid: u32,
}

#[derive(Serialize)]
struct StopRequest {
    version: u32,
    command: &'static str,
}

pub struct MacOsBackend {
    app_handle: AppHandle,
}

impl MacOsBackend {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    fn helper_path(&self) -> AppResult<PathBuf> {
        resolve_helper_path(&self.app_handle).ok_or_else(|| {
            AppError::Other(
                "macOS patcher helper is missing. Run `pnpm macos:helper` and restart LTK Manager."
                    .into(),
            )
        })
    }
}

impl PatcherBackend for MacOsBackend {
    fn name(&self) -> &'static str {
        "macos-arm64-helper"
    }

    fn availability(&self) -> PatcherAvailability {
        if std::env::consts::ARCH != "aarch64" {
            return PatcherAvailability::unsupported(
                "The initial macOS patcher supports Apple Silicon and an ARM64 League process only",
            );
        }
        match self.helper_path() {
            Ok(_) => PatcherAvailability {
                supported: true,
                ready: true,
                reason: Some(
                    "macOS will request administrator approval when a patcher session starts"
                        .into(),
                ),
                requires_setup: false,
                permission_required: true,
                helper_version: Some(MACOS_HELPER_VERSION.into()),
            },
            Err(error) => PatcherAvailability {
                supported: true,
                ready: false,
                reason: Some(error.to_string()),
                requires_setup: true,
                permission_required: true,
                helper_version: None,
            },
        }
    }

    fn preflight(&self, context: &PatcherContext) -> AppResult<PatcherPreflight> {
        let helper = self.helper_path()?;
        let output = Command::new(helper)
            .arg("--preflight")
            .arg(&context.league_install.game_executable)
            .output()
            .map_err(|error| {
                AppError::Other(format!("Failed to run macOS helper preflight: {error}"))
            })?;
        let line = String::from_utf8_lossy(&output.stdout);
        let event: HelperEvent = serde_json::from_str(line.trim()).map_err(|error| {
            AppError::Other(format!("Invalid macOS helper preflight response: {error}"))
        })?;
        Ok(PatcherPreflight {
            compatible: output.status.success() && event.event == "compatible",
            backend: self.name().into(),
            architecture: event
                .architecture
                .unwrap_or_else(|| std::env::consts::ARCH.into()),
            signature: event.signature,
            reason: event.detail,
        })
    }

    fn run(
        &self,
        context: PatcherContext,
        stop: Arc<AtomicBool>,
        events: PatcherEventSink,
    ) -> BackendResult<()> {
        let helper = self.helper_path().map_err(|error| BackendError::Failed {
            code: "HELPER_MISSING".into(),
            detail: error.to_string(),
        })?;
        let game_bundle = &context.league_install.install_root;

        let uid = unsafe { libc::getuid() };
        let session_dir = std::env::temp_dir().join(format!(
            "dev.leaguetoolkit.manager-{}-{}",
            uid,
            Uuid::new_v4()
        ));
        fs::create_dir(&session_dir).map_err(|error| BackendError::Failed {
            code: "HELPER_SESSION_FAILED".into(),
            detail: format!("Failed to create helper session directory: {error}"),
        })?;
        fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            BackendError::Failed {
                code: "HELPER_SESSION_FAILED".into(),
                detail: format!("Failed to secure helper session directory: {error}"),
            }
        })?;
        let _cleanup = SessionCleanup(session_dir.clone());

        let socket_path = session_dir.join("control.sock");
        let listener = UnixListener::bind(&socket_path).map_err(|error| BackendError::Failed {
            code: "HELPER_SESSION_FAILED".into(),
            detail: format!("Failed to create helper control socket: {error}"),
        })?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            BackendError::Failed {
                code: "HELPER_SESSION_FAILED".into(),
                detail: format!("Failed to secure helper control socket: {error}"),
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| BackendError::Failed {
                code: "HELPER_SESSION_FAILED".into(),
                detail: format!("Failed to configure helper control socket: {error}"),
            })?;

        let token = Uuid::new_v4().to_string();
        let mut child = ChildGuard::new(launch_elevated_helper(&helper, &socket_path, &token)?);
        let stream = accept_helper(&listener, &mut child, &stop)?;
        stream
            .set_read_timeout(Some(IO_POLL_INTERVAL))
            .map_err(|error| BackendError::Failed {
                code: "HELPER_SESSION_FAILED".into(),
                detail: format!("Failed to configure helper socket timeout: {error}"),
            })?;
        let mut writer = stream.try_clone().map_err(|error| BackendError::Failed {
            code: "HELPER_SESSION_FAILED".into(),
            detail: format!("Failed to clone helper socket: {error}"),
        })?;
        let mut reader = BufReader::new(stream);

        let hello = read_helper_event(&mut reader)?.ok_or_else(|| BackendError::Failed {
            code: "HELPER_PROTOCOL_ERROR".into(),
            detail: "Helper disconnected before authentication".into(),
        })?;
        if hello.version != PROTOCOL_VERSION
            || hello.event != "hello"
            || hello.token.as_deref() != Some(token.as_str())
            || hello.helper_version.as_deref() != Some(MACOS_HELPER_VERSION)
            || hello.architecture.as_deref() != Some("aarch64")
        {
            return Err(BackendError::Failed {
                code: "HELPER_VERSION_MISMATCH".into(),
                detail:
                    "The bundled helper failed protocol, token, version, or architecture validation"
                        .into(),
            });
        }

        write_request(
            &mut writer,
            &StartRequest {
                version: PROTOCOL_VERSION,
                command: "start",
                token: &token,
                overlay: &context.overlay_root,
                allowed_root: &context.allowed_root,
                game_bundle,
                game_executable: &context.league_install.game_executable,
                client_uid: uid,
            },
        )?;

        let mut stop_sent = false;
        loop {
            if stop.load(Ordering::SeqCst) && !stop_sent {
                write_request(
                    &mut writer,
                    &StopRequest {
                        version: PROTOCOL_VERSION,
                        command: "stop",
                    },
                )?;
                stop_sent = true;
            }

            match read_helper_event(&mut reader) {
                Ok(Some(event)) => {
                    if event.version != PROTOCOL_VERSION {
                        return Err(BackendError::Failed {
                            code: "HELPER_PROTOCOL_ERROR".into(),
                            detail: format!(
                                "Unsupported helper protocol version {}",
                                event.version
                            ),
                        });
                    }
                    if event.event == "error" {
                        return Err(BackendError::Failed {
                            code: event.code.unwrap_or_else(|| "PATCHER_HELPER_FAILED".into()),
                            detail: event
                                .detail
                                .unwrap_or_else(|| "The macOS helper reported an error".into()),
                        });
                    }
                    events(BackendEvent {
                        event: event.event.clone(),
                        pid: event.pid,
                        architecture: event.architecture,
                        signature: event.signature,
                        detail: event.detail,
                    });
                    if event.event == "stopped" {
                        child.wait_bounded();
                        return if stop_sent {
                            Err(BackendError::Stopped)
                        } else {
                            Ok(())
                        };
                    }
                }
                Ok(None) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(BackendError::Failed {
                            code: "HELPER_EXITED".into(),
                            detail: format!(
                                "The elevated helper exited unexpectedly with status {status}"
                            ),
                        });
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

pub fn resolve_helper_path(app_handle: &AppHandle) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(directory) = current_exe.parent() {
            candidates.push(directory.join(HELPER_NAME));
            candidates.push(directory.join(HELPER_TARGET_NAME));
        }
    }
    if let Ok(resource_dir) = app_handle.path().resource_dir() {
        candidates.push(resource_dir.join(HELPER_NAME));
        candidates.push(resource_dir.join(HELPER_TARGET_NAME));
        candidates.push(resource_dir.join("binaries").join(HELPER_NAME));
        candidates.push(resource_dir.join("binaries").join(HELPER_TARGET_NAME));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(HELPER_TARGET_NAME),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("debug")
            .join(HELPER_NAME),
    );

    candidates
        .into_iter()
        .find(|path| path.is_file() && is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn launch_elevated_helper(helper: &Path, socket: &Path, token: &str) -> BackendResult<Child> {
    if unsafe { libc::geteuid() } == 0
        || std::env::var_os("LTK_MACOS_PATCHER_NO_ELEVATION").is_some()
    {
        return Command::new(helper)
            .args(["--socket"])
            .arg(socket)
            .args(["--token", token])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| BackendError::Failed {
                code: "HELPER_LAUNCH_FAILED".into(),
                detail: format!("Failed to launch macOS helper: {error}"),
            });
    }

    let command = format!(
        "{} --socket {} --token {}",
        shell_quote(helper.as_os_str().to_string_lossy().as_ref()),
        shell_quote(socket.as_os_str().to_string_lossy().as_ref()),
        shell_quote(token)
    );
    let apple_script = format!(
        "do shell script \"{}\" with administrator privileges",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    Command::new("/usr/bin/osascript")
        .args(["-e", &apple_script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| BackendError::Failed {
            code: "HELPER_LAUNCH_FAILED".into(),
            detail: format!("Failed to request macOS administrator approval: {error}"),
        })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn accept_helper(
    listener: &UnixListener,
    child: &mut Child,
    stop: &AtomicBool,
) -> BackendResult<UnixStream> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            return Err(BackendError::Stopped);
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(BackendError::Failed {
                    code: "HELPER_SESSION_FAILED".into(),
                    detail: format!("Failed to accept helper connection: {error}"),
                });
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(BackendError::Failed {
                code: "HELPER_AUTHORIZATION_DENIED".into(),
                detail: format!(
                    "The helper did not start. Administrator approval may have been cancelled ({status})."
                ),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(BackendError::Failed {
                code: "HELPER_START_TIMEOUT".into(),
                detail: "Timed out waiting for the elevated macOS helper".into(),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read_helper_event(reader: &mut BufReader<UnixStream>) -> BackendResult<Option<HelperEvent>> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) if line.len() > 64 * 1024 => Err(BackendError::Failed {
            code: "HELPER_PROTOCOL_ERROR".into(),
            detail: "Helper response exceeded 64 KiB".into(),
        }),
        Ok(_) => serde_json::from_str(&line)
            .map(Some)
            .map_err(|error| BackendError::Failed {
                code: "HELPER_PROTOCOL_ERROR".into(),
                detail: format!("Malformed helper event: {error}"),
            }),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Ok(None)
        }
        Err(error) => Err(BackendError::Failed {
            code: "HELPER_PROTOCOL_ERROR".into(),
            detail: format!("Failed to read helper event: {error}"),
        }),
    }
}

fn write_request(writer: &mut UnixStream, request: &impl Serialize) -> BackendResult<()> {
    serde_json::to_writer(&mut *writer, request).map_err(|error| BackendError::Failed {
        code: "HELPER_PROTOCOL_ERROR".into(),
        detail: format!("Failed to encode helper request: {error}"),
    })?;
    writer
        .write_all(b"\n")
        .and_then(|_| writer.flush())
        .map_err(|error| BackendError::Failed {
            code: "HELPER_PROTOCOL_ERROR".into(),
            detail: format!("Failed to send helper request: {error}"),
        })
}

fn wait_for_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct ChildGuard {
    child: Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn wait_bounded(&mut self) {
        wait_for_child(&mut self.child);
        self.reaped = true;
    }
}

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct SessionCleanup(PathBuf);

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_handles_apostrophes() {
        assert_eq!(shell_quote("/tmp/a'b"), "'/tmp/a'\\''b'");
    }
}
