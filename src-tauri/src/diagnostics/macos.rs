use super::{check, check_ok, Category, Check, CheckCtx, CheckDetail, Severity};
use serde::Deserialize;
use std::process::Command;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreflightEvent {
    event: String,
    code: Option<String>,
    detail: Option<String>,
    signature: Option<String>,
    architecture: Option<String>,
    helper_version: Option<String>,
}

pub fn system_checks(ctx: &CheckCtx) -> Vec<Check> {
    vec![
        check_version(),
        check_architecture(),
        check_sip(),
        check_translocation(ctx),
        check_quarantine(ctx),
    ]
}

pub fn patcher_checks(ctx: &CheckCtx) -> Vec<Check> {
    vec![
        check_helper(ctx),
        check_game_architecture_and_signature(ctx),
        check_process_access_model(),
    ]
}

fn check_version() -> Check {
    let version = command_output("/usr/bin/sw_vers", &["-productVersion"]);
    let Some(version) = version else {
        return check(
            "macos.version",
            "macOS version",
            Category::System,
            Severity::Warn,
            "Could not determine macOS version",
        );
    };
    let major = version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    if major.is_some_and(|major| major >= 13) {
        check_ok(
            "macos.version",
            "macOS version",
            Category::System,
            &format!("macOS {version}"),
        )
    } else {
        let mut result = check(
            "macos.version",
            "macOS version",
            Category::System,
            Severity::Bad,
            format!("macOS {version} is below the supported minimum"),
        );
        result.suggestion = Some("Upgrade this Mac to macOS 13 or newer.".into());
        result
    }
}

fn check_architecture() -> Check {
    if std::env::consts::ARCH == "aarch64" {
        check_ok(
            "macos.architecture",
            "Manager architecture",
            Category::System,
            "Apple Silicon (ARM64)",
        )
    } else {
        check(
            "macos.architecture",
            "Manager architecture",
            Category::System,
            Severity::Bad,
            format!(
                "{} is unsupported; this implementation requires ARM64",
                std::env::consts::ARCH
            ),
        )
    }
}

fn check_sip() -> Check {
    let output = command_output("/usr/bin/csrutil", &["status"]);
    match output {
        Some(status) if status.to_ascii_lowercase().contains("enabled") => check_ok(
            "macos.sip",
            "System Integrity Protection",
            Category::System,
            "Enabled",
        ),
        Some(status) if status.to_ascii_lowercase().contains("disabled") => {
            let mut result = check(
                "macos.sip",
                "System Integrity Protection",
                Category::System,
                Severity::Bad,
                "Disabled",
            );
            result.suggestion = Some(
                "LTK Manager is designed to work with SIP enabled. Re-enable SIP before using the patcher."
                    .into(),
            );
            result
        }
        _ => check(
            "macos.sip",
            "System Integrity Protection",
            Category::System,
            Severity::Warn,
            "Could not determine SIP status",
        ),
    }
}

fn check_translocation(ctx: &CheckCtx) -> Check {
    let translocated = ctx
        .manager_exe
        .as_ref()
        .is_some_and(|path| path.to_string_lossy().contains("/AppTranslocation/"));
    if translocated {
        let mut result = check(
            "macos.translocation",
            "App Translocation",
            Category::Manager,
            Severity::Bad,
            "LTK Manager is running from an App Translocation path",
        );
        result.suggestion = Some(
            "Move LTK Manager into /Applications and launch it from there before setting up the helper."
                .into(),
        );
        result
    } else {
        check_ok(
            "macos.translocation",
            "App Translocation",
            Category::Manager,
            "Not translocated",
        )
    }
}

fn check_quarantine(ctx: &CheckCtx) -> Check {
    let Some(executable) = ctx.manager_exe.as_ref() else {
        return check(
            "macos.quarantine",
            "Quarantine attribute",
            Category::Manager,
            Severity::Info,
            "Manager executable path unavailable",
        );
    };
    let output = Command::new("/usr/bin/xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(executable)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let mut result = check(
                "macos.quarantine",
                "Quarantine attribute",
                Category::Manager,
                Severity::Warn,
                "Present",
            );
            result.suggestion = Some(
                "If helper setup or launch fails, move the app to /Applications and clear quarantine only after verifying the build source."
                    .into(),
            );
            result
        }
        _ => check_ok(
            "macos.quarantine",
            "Quarantine attribute",
            Category::Manager,
            "Not present",
        ),
    }
}

fn check_helper(ctx: &CheckCtx) -> Check {
    let Some(helper) = ctx.patcher_helper_path.as_ref() else {
        let mut result = check(
            "macos.helper.present",
            "Native patcher helper",
            Category::Patcher,
            Severity::Bad,
            "Missing",
        );
        result.suggestion = Some("Run `pnpm macos:helper`, then restart LTK Manager.".into());
        return result;
    };
    let mut result = check_ok(
        "macos.helper.present",
        "Native patcher helper",
        Category::Patcher,
        "Bundled and executable",
    );
    result
        .details
        .push(CheckDetail::new("path", helper.display().to_string()));
    result
}

fn check_game_architecture_and_signature(ctx: &CheckCtx) -> Check {
    let (Some(helper), Some(install)) = (
        ctx.patcher_helper_path.as_ref(),
        ctx.league_install.as_ref(),
    ) else {
        return check(
            "macos.patcher.preflight",
            "ARM64 patch signature",
            Category::Patcher,
            Severity::Info,
            "Helper or League installation unavailable",
        );
    };
    let output = Command::new(helper)
        .arg("--preflight")
        .arg(&install.game_executable)
        .output();
    let Ok(output) = output else {
        return check(
            "macos.patcher.preflight",
            "ARM64 patch signature",
            Category::Patcher,
            Severity::Bad,
            "Failed to execute helper preflight",
        );
    };
    let event = serde_json::from_slice::<PreflightEvent>(&output.stdout);
    match event {
        Ok(event) if output.status.success() && event.event == "compatible" => {
            let mut result = check_ok(
                "macos.patcher.preflight",
                "ARM64 patch signature",
                Category::Patcher,
                "Compatible",
            );
            if let Some(architecture) = event.architecture {
                result
                    .details
                    .push(CheckDetail::new("architecture", architecture));
            }
            if let Some(signature) = event.signature {
                result
                    .details
                    .push(CheckDetail::new("signature", signature));
            }
            if let Some(version) = event.helper_version {
                result
                    .details
                    .push(CheckDetail::new("helper_version", version));
            }
            result
        }
        Ok(event) => {
            let mut result = check(
                "macos.patcher.preflight",
                "ARM64 patch signature",
                Category::Patcher,
                Severity::Bad,
                event
                    .detail
                    .unwrap_or_else(|| "Current League build is unsupported".into()),
            );
            if let Some(code) = event.code {
                result.details.push(CheckDetail::new("code", code));
            }
            result.suggestion = Some(
                "Do not start patching until a signature update supports this League build.".into(),
            );
            result
        }
        Err(error) => {
            let mut result = check(
                "macos.patcher.preflight",
                "ARM64 patch signature",
                Category::Patcher,
                Severity::Bad,
                "Helper returned an invalid preflight response",
            );
            result
                .details
                .push(CheckDetail::new("error", error.to_string()));
            result
        }
    }
}

fn check_process_access_model() -> Check {
    let mut result = check(
        "macos.patcher.authorization",
        "Process access authorization",
        Category::Patcher,
        Severity::Info,
        "Administrator approval is requested per patcher session",
    );
    result.suggestion = Some(
        "Approve the macOS prompt when starting the patcher. LTK Manager itself remains unprivileged."
            .into(),
    );
    result
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
