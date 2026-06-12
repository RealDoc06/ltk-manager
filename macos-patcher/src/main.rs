use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const PROTOCOL_VERSION: u32 = 1;
const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest {
    version: u32,
    command: String,
    token: String,
    overlay: PathBuf,
    allowed_root: PathBuf,
    game_bundle: PathBuf,
    game_executable: PathBuf,
    client_uid: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlRequest {
    version: u32,
    command: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Event<'a> {
    version: u32,
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    architecture: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    helper_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
extern "C" {
    fn ltk_macos_preflight(
        game_executable: *const c_char,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
    fn ltk_macos_run(
        overlay: *const c_char,
        game_executable: *const c_char,
        context: *mut c_void,
        event_callback: extern "C" fn(*mut c_void, *const c_char, u32, *const c_char),
        stop_callback: extern "C" fn(*mut c_void) -> bool,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
    #[cfg(test)]
    fn ltk_macos_test_find_unique_wad_verify(
        text: *const u8,
        text_len: usize,
        text_address: u64,
        result: *mut u64,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
    #[cfg(test)]
    fn ltk_macos_test_parse_arm64_text(
        data: *const u8,
        data_len: usize,
        address: *mut u64,
        size: *mut usize,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
}

struct CallbackContext {
    writer: Arc<Mutex<UnixStream>>,
    stop: Arc<AtomicBool>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_, flag, executable] if flag == "--preflight" => run_preflight(Path::new(executable)),
        [_, socket_flag, socket, token_flag, token]
            if socket_flag == "--socket" && token_flag == "--token" =>
        {
            run_socket(Path::new(socket), token)
        }
        _ => Err(
            "usage: ltk-macos-patcher --preflight <game-executable> | --socket <path> --token <token>"
                .into(),
        ),
    }
}

fn run_preflight(executable: &Path) -> Result<(), String> {
    let result = preflight(executable);
    let (event, code, detail, signature) = match &result {
        Ok(signature) => ("compatible", None, None, Some(signature.as_str())),
        Err(error) => (
            "error",
            Some("UNSUPPORTED_GAME_BUILD"),
            Some(error.as_str()),
            None,
        ),
    };
    let payload = Event {
        version: PROTOCOL_VERSION,
        event,
        code,
        detail,
        signature,
        architecture: Some("arm64"),
        pid: None,
        helper_version: Some(HELPER_VERSION),
        token: None,
    };
    println!(
        "{}",
        serde_json::to_string(&payload).map_err(|error| error.to_string())?
    );
    result.map(|_| ())
}

fn run_socket(socket_path: &Path, expected_token: &str) -> Result<(), String> {
    let socket_metadata =
        fs::metadata(socket_path).map_err(|error| format!("invalid socket path: {error}"))?;
    if !socket_metadata.file_type().is_socket() {
        return Err("control path is not a Unix socket".into());
    }

    let stream =
        UnixStream::connect(socket_path).map_err(|error| format!("connect failed: {error}"))?;
    let writer = Arc::new(Mutex::new(
        stream
            .try_clone()
            .map_err(|error| format!("socket clone failed: {error}"))?,
    ));
    send_event(
        &writer,
        Event {
            version: PROTOCOL_VERSION,
            event: "hello",
            code: None,
            detail: None,
            signature: None,
            architecture: Some(std::env::consts::ARCH),
            pid: None,
            helper_version: Some(HELPER_VERSION),
            token: Some(expected_token),
        },
    )?;

    let mut reader = BufReader::new(stream);
    let request: StartRequest = match read_json_line(&mut reader) {
        Ok(request) => request,
        Err(error) => {
            send_event(
                &writer,
                Event {
                    version: PROTOCOL_VERSION,
                    event: "error",
                    code: Some("HELPER_PROTOCOL_ERROR"),
                    detail: Some(&error),
                    signature: None,
                    architecture: Some("arm64"),
                    pid: None,
                    helper_version: Some(HELPER_VERSION),
                    token: None,
                },
            )?;
            return Err(error);
        }
    };
    validate_start_request(&request, expected_token, socket_metadata.uid())?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = stop.clone();
    std::thread::spawn(move || loop {
        match read_json_line::<ControlRequest, _>(&mut reader) {
            Ok(request) if request.version == PROTOCOL_VERSION && request.command == "stop" => {
                stop_reader.store(true, Ordering::SeqCst);
                return;
            }
            Ok(_) => {}
            Err(_) => {
                stop_reader.store(true, Ordering::SeqCst);
                return;
            }
        }
    });

    let overlay = path_to_cstring(&request.overlay)?;
    let executable = path_to_cstring(&request.game_executable)?;
    let mut callback_context = CallbackContext {
        writer: writer.clone(),
        stop,
    };
    let mut error = vec![0_i8; 2048];

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let result = unsafe {
        ltk_macos_run(
            overlay.as_ptr(),
            executable.as_ptr(),
            (&mut callback_context as *mut CallbackContext).cast(),
            native_event_callback,
            native_stop_callback,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let result = {
        write_error(&mut error, "helper requires macOS on Apple Silicon");
        -1
    };

    if result != 0 {
        let detail = c_buffer_to_string(&error);
        let code = classify_native_error(&detail);
        send_event(
            &writer,
            Event {
                version: PROTOCOL_VERSION,
                event: "error",
                code: Some(code),
                detail: Some(&detail),
                signature: None,
                architecture: Some("arm64"),
                pid: None,
                helper_version: Some(HELPER_VERSION),
                token: None,
            },
        )?;
        return Err(detail);
    }

    send_event(
        &writer,
        Event {
            version: PROTOCOL_VERSION,
            event: "stopped",
            code: None,
            detail: None,
            signature: None,
            architecture: Some("arm64"),
            pid: None,
            helper_version: Some(HELPER_VERSION),
            token: None,
        },
    )
}

fn validate_start_request(
    request: &StartRequest,
    expected_token: &str,
    socket_uid: u32,
) -> Result<(), String> {
    let game_executable =
        validate_start_request_authorization(request, expected_token, socket_uid, unsafe {
            libc::geteuid()
        })?;
    preflight(&game_executable)?;
    Ok(())
}

fn validate_start_request_authorization(
    request: &StartRequest,
    expected_token: &str,
    socket_uid: u32,
    effective_uid: u32,
) -> Result<PathBuf, String> {
    if request.version != PROTOCOL_VERSION {
        return Err(format!("unsupported protocol version {}", request.version));
    }
    if request.command != "start" {
        return Err("first command must be start".into());
    }
    if request.token != expected_token {
        return Err("authentication token mismatch".into());
    }
    if request.client_uid != socket_uid {
        return Err("socket owner does not match requesting user".into());
    }
    if effective_uid != 0 {
        return Err(
            "patcher helper must be elevated, but the Tauri app must remain unprivileged".into(),
        );
    }

    let allowed_root = canonical_directory(&request.allowed_root, "allowed root")?;
    let overlay = canonical_directory(&request.overlay, "overlay")?;
    if !overlay.starts_with(&allowed_root) {
        return Err("overlay is outside the approved LTK data directory".into());
    }
    if fs::metadata(&allowed_root)
        .map_err(|error| format!("cannot inspect allowed root: {error}"))?
        .uid()
        != request.client_uid
    {
        return Err("approved LTK data directory is not owned by the requesting user".into());
    }

    let game_bundle = canonical_directory(&request.game_bundle, "game bundle")?;
    if game_bundle.extension().and_then(|ext| ext.to_str()) != Some("app") {
        return Err("game bundle is not an application bundle".into());
    }
    let game_executable = fs::canonicalize(&request.game_executable)
        .map_err(|error| format!("invalid game executable: {error}"))?;
    if !game_executable.is_file() || !game_executable.starts_with(&game_bundle) {
        return Err("game executable is outside the configured League bundle".into());
    }
    if fs::metadata(&game_executable)
        .map_err(|error| format!("cannot inspect game executable: {error}"))?
        .uid()
        != request.client_uid
    {
        return Err("game executable is not owned by the requesting user".into());
    }
    Ok(game_executable)
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|error| format!("invalid {label}: {error}"))?;
    if !path.is_dir() {
        return Err(format!("{label} is not a directory"));
    }
    Ok(path)
}

fn read_json_line<T: for<'de> Deserialize<'de>, R: BufRead>(reader: &mut R) -> Result<T, String> {
    let mut line = String::new();
    reader
        .take(MAX_REQUEST_BYTES)
        .read_line(&mut line)
        .map_err(|error| format!("failed to read request: {error}"))?;
    if line.is_empty() || !line.ends_with('\n') {
        return Err("request is empty, oversized, or not newline terminated".into());
    }
    serde_json::from_str(&line).map_err(|error| format!("malformed request: {error}"))
}

fn preflight(executable: &Path) -> Result<String, String> {
    let executable = fs::canonicalize(executable)
        .map_err(|error| format!("invalid game executable: {error}"))?;
    validate_riot_code_signature(&executable)?;
    let executable = path_to_cstring(&executable)?;
    let mut error = vec![0_i8; 2048];

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let result =
        unsafe { ltk_macos_preflight(executable.as_ptr(), error.as_mut_ptr(), error.len()) };
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let result = {
        write_error(&mut error, "helper requires macOS on Apple Silicon");
        -1
    };

    if result == 0 {
        Ok("mac-arm64-pattern-v1".into())
    } else {
        Err(c_buffer_to_string(&error))
    }
}

fn validate_riot_code_signature(executable: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let verification = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--ignore-resources", "--verbose=1"])
            .arg(executable)
            .output()
            .map_err(|error| format!("failed to verify League code signature: {error}"))?;
        if !verification.status.success() {
            return Err(format!(
                "League executable code signature is invalid: {}",
                String::from_utf8_lossy(&verification.stderr).trim()
            ));
        }

        let details = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(executable)
            .output()
            .map_err(|error| format!("failed to inspect League code signature: {error}"))?;
        let details = String::from_utf8_lossy(&details.stderr);
        if !details.contains("Identifier=com.riotgames.LeagueofLegends.GameClient")
            || !details.contains("TeamIdentifier=K832E2UXV7")
        {
            return Err("League executable is not signed with the expected Riot identity".into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = executable;
        Err("helper requires macOS on Apple Silicon".into())
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("path contains a NUL byte: {}", path.display()))
}

fn c_buffer_to_string(buffer: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn classify_native_error(detail: &str) -> &'static str {
    if detail.contains("task_for_pid") {
        "PROCESS_ACCESS_DENIED"
    } else if detail.contains("ARM64") || detail.contains("architecture") {
        "UNSUPPORTED_ARCHITECTURE"
    } else if detail.contains("signature")
        || detail.contains("wad_verify")
        || detail.contains("fopen")
    {
        "UNSUPPORTED_GAME_BUILD"
    } else {
        "PATCHER_HELPER_FAILED"
    }
}

fn send_event(writer: &Arc<Mutex<UnixStream>>, event: Event<'_>) -> Result<(), String> {
    let mut writer = writer
        .lock()
        .map_err(|_| "event writer lock is poisoned".to_string())?;
    serde_json::to_writer(&mut *writer, &event).map_err(|error| error.to_string())?;
    writer
        .write_all(b"\n")
        .and_then(|_| writer.flush())
        .map_err(|error| format!("event write failed: {error}"))
}

extern "C" fn native_event_callback(
    context: *mut c_void,
    event: *const c_char,
    pid: u32,
    detail: *const c_char,
) {
    if context.is_null() || event.is_null() {
        return;
    }
    let context = unsafe { &*(context.cast::<CallbackContext>()) };
    let event = unsafe { CStr::from_ptr(event) }.to_string_lossy();
    let detail = if detail.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(detail) }.to_string_lossy())
    };
    let _ = send_event(
        &context.writer,
        Event {
            version: PROTOCOL_VERSION,
            event: &event,
            code: None,
            detail: detail.as_deref(),
            signature: (event == "patched").then_some("mac-arm64-pattern-v1"),
            architecture: (event == "gameFound").then_some("arm64"),
            pid: (pid != 0).then_some(pid),
            helper_version: Some(HELPER_VERSION),
            token: None,
        },
    );
}

extern "C" fn native_stop_callback(context: *mut c_void) -> bool {
    if context.is_null() {
        return true;
    }
    unsafe { &*(context.cast::<CallbackContext>()) }
        .stop
        .load(Ordering::SeqCst)
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn write_error(buffer: &mut [c_char], message: &str) {
    let bytes = message.as_bytes();
    let count = bytes.len().min(buffer.len().saturating_sub(1));
    for (target, source) in buffer.iter_mut().zip(bytes.iter()).take(count) {
        *target = *source as c_char;
    }
    if let Some(terminator) = buffer.get_mut(count) {
        *terminator = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::fs::{symlink, MetadataExt};

    fn request_fixture(root: &Path) -> StartRequest {
        let allowed_root = root.join("data");
        let overlay = allowed_root.join("overlay");
        let game_bundle = root.join("League of Legends.app");
        let game_executable = game_bundle
            .join("Contents/LoL/Game/LeagueofLegends.app/Contents/MacOS/LeagueofLegends");
        fs::create_dir_all(&overlay).unwrap();
        fs::create_dir_all(game_executable.parent().unwrap()).unwrap();
        fs::write(&game_executable, b"fixture").unwrap();
        let client_uid = fs::metadata(&allowed_root).unwrap().uid();
        StartRequest {
            version: PROTOCOL_VERSION,
            command: "start".into(),
            token: "secret".into(),
            overlay,
            allowed_root,
            game_bundle,
            game_executable,
            client_uid,
        }
    }

    fn append_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn append_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn append_name(bytes: &mut Vec<u8>, value: &str) {
        let mut name = [0_u8; 16];
        name[..value.len()].copy_from_slice(value.as_bytes());
        bytes.extend_from_slice(&name);
    }

    fn thin_arm64_macho() -> Vec<u8> {
        const HEADER_SIZE: u32 = 32;
        const SEGMENT_COMMAND_SIZE: u32 = 8 + 64 + 80;
        const TEXT_SIZE: u64 = 16;
        let text_offset = HEADER_SIZE + SEGMENT_COMMAND_SIZE;
        let mut bytes = Vec::new();

        append_u32(&mut bytes, 0xFEED_FACF);
        append_u32(&mut bytes, 0x0100_000C);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 2);
        append_u32(&mut bytes, 1);
        append_u32(&mut bytes, SEGMENT_COMMAND_SIZE);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);

        append_u32(&mut bytes, 0x19);
        append_u32(&mut bytes, SEGMENT_COMMAND_SIZE);
        append_name(&mut bytes, "__TEXT");
        append_u64(&mut bytes, 0x1_0000_0000);
        append_u64(&mut bytes, 0x1000);
        append_u64(&mut bytes, 0);
        append_u64(&mut bytes, u64::from(text_offset) + TEXT_SIZE);
        append_u32(&mut bytes, 5);
        append_u32(&mut bytes, 5);
        append_u32(&mut bytes, 1);
        append_u32(&mut bytes, 0);

        append_name(&mut bytes, "__text");
        append_name(&mut bytes, "__TEXT");
        append_u64(&mut bytes, 0x1_0000_1000);
        append_u64(&mut bytes, TEXT_SIZE);
        append_u32(&mut bytes, text_offset);
        append_u32(&mut bytes, 2);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);
        append_u32(&mut bytes, 0);

        bytes.extend_from_slice(&[0xAA; TEXT_SIZE as usize]);
        bytes
    }

    fn universal_with_arm64_slice(thin: &[u8]) -> Vec<u8> {
        const FAT_HEADER_SIZE: usize = 8 + 20;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xCAFE_BABE_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&0x0100_000C_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&(FAT_HEADER_SIZE as u32).to_be_bytes());
        bytes.extend_from_slice(&(thin.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(thin);
        bytes
    }

    #[test]
    fn classifies_native_failures() {
        assert_eq!(
            classify_native_error("task_for_pid failed"),
            "PROCESS_ACCESS_DENIED"
        );
        assert_eq!(
            classify_native_error("wad_verify signature not unique"),
            "UNSUPPORTED_GAME_BUILD"
        );
    }

    #[test]
    fn authorization_accepts_owned_paths_inside_approved_roots() {
        let directory = tempfile::tempdir().unwrap();
        let request = request_fixture(directory.path());
        let executable =
            validate_start_request_authorization(&request, "secret", request.client_uid, 0)
                .unwrap();
        assert_eq!(
            executable,
            fs::canonicalize(&request.game_executable).unwrap()
        );
    }

    #[test]
    fn authorization_rejects_token_uid_and_non_root_helper() {
        let directory = tempfile::tempdir().unwrap();
        let request = request_fixture(directory.path());
        assert!(
            validate_start_request_authorization(&request, "wrong", request.client_uid, 0)
                .unwrap_err()
                .contains("token")
        );
        assert!(validate_start_request_authorization(
            &request,
            "secret",
            request.client_uid + 1,
            0
        )
        .unwrap_err()
        .contains("socket owner"));
        assert!(validate_start_request_authorization(
            &request,
            "secret",
            request.client_uid,
            request.client_uid.max(1)
        )
        .unwrap_err()
        .contains("must be elevated"));
    }

    #[test]
    fn authorization_rejects_overlay_symlink_escape() {
        let directory = tempfile::tempdir().unwrap();
        let request = request_fixture(directory.path());
        let outside = directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::remove_dir(&request.overlay).unwrap();
        symlink(&outside, &request.overlay).unwrap();
        assert!(
            validate_start_request_authorization(&request, "secret", request.client_uid, 0)
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn authorization_rejects_executable_outside_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let mut request = request_fixture(directory.path());
        request.game_executable = directory.path().join("LeagueofLegends");
        fs::write(&request.game_executable, b"fixture").unwrap();
        assert!(
            validate_start_request_authorization(&request, "secret", request.client_uid, 0)
                .unwrap_err()
                .contains("outside the configured League bundle")
        );
    }

    #[test]
    fn protocol_rejects_malformed_and_oversized_requests() {
        let mut malformed = Cursor::new(b"{not-json}\n".to_vec());
        assert!(read_json_line::<ControlRequest, _>(&mut malformed)
            .unwrap_err()
            .contains("malformed"));

        let mut oversized = Cursor::new(vec![b'x'; MAX_REQUEST_BYTES as usize + 1]);
        assert!(read_json_line::<ControlRequest, _>(&mut oversized)
            .unwrap_err()
            .contains("oversized"));
    }

    #[test]
    fn stop_callback_observes_cancellation() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let context = CallbackContext {
            writer: Arc::new(Mutex::new(stream)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        assert!(!native_stop_callback(
            (&context as *const CallbackContext).cast_mut().cast()
        ));
        context.stop.store(true, Ordering::SeqCst);
        assert!(native_stop_callback(
            (&context as *const CallbackContext).cast_mut().cast()
        ));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn native_signature_matcher_requires_exactly_one_valid_match() {
        const PATTERN: [u8; 8] = [0xC3, 0x24, 0x80, 0x52, 0x04, 0x20, 0x80, 0x52];
        let mut text = [0_u8; 32];
        text[4..12].copy_from_slice(&PATTERN);
        text[12..16].copy_from_slice(&0x14000002_u32.to_le_bytes());
        let mut result = 0_u64;
        let mut error = vec![0_i8; 256];
        let status = unsafe {
            ltk_macos_test_find_unique_wad_verify(
                text.as_ptr(),
                text.len(),
                0x1000,
                &mut result,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_eq!(status, 0);
        assert_eq!(result, 0x1014);

        text[20..28].copy_from_slice(&PATTERN);
        let status = unsafe {
            ltk_macos_test_find_unique_wad_verify(
                text.as_ptr(),
                text.len(),
                0x1000,
                &mut result,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_ne!(status, 0);
        assert!(c_buffer_to_string(&error).contains("exactly one match"));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn native_macho_parser_selects_thin_and_universal_arm64_text() {
        let thin = thin_arm64_macho();
        let universal = universal_with_arm64_slice(&thin);
        for fixture in [&thin, &universal] {
            let mut address = 0_u64;
            let mut size = 0_usize;
            let mut error = vec![0_i8; 256];
            let status = unsafe {
                ltk_macos_test_parse_arm64_text(
                    fixture.as_ptr(),
                    fixture.len(),
                    &mut address,
                    &mut size,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(status, 0, "{}", c_buffer_to_string(&error));
            assert_eq!(address, 0x1_0000_1000);
            assert_eq!(size, 16);
        }
    }
}
