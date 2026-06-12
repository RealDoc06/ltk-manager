use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

const WINDOWS_GAME_EXE: &str = "League of Legends.exe";
const MAC_GAME_APP: &str = "LeagueofLegends.app";
const MAC_GAME_EXE: &str = "LeagueofLegends";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaguePlatform {
    Windows,
    MacOs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeagueInstall {
    pub configured_path: PathBuf,
    pub install_root: PathBuf,
    pub game_dir: PathBuf,
    pub client_lockfile: PathBuf,
    pub game_executable: PathBuf,
    pub game_bundle: Option<PathBuf>,
    pub platform: LeaguePlatform,
}

impl LeagueInstall {
    pub fn resolve(path: impl AsRef<Path>) -> AppResult<Self> {
        let configured_path = path.as_ref();

        if let Some(install) = resolve_windows(configured_path) {
            return Ok(install);
        }
        if let Some(install) = resolve_macos(configured_path) {
            return Ok(install);
        }

        Err(AppError::ValidationFailed(format!(
            "League path is not a supported installation, Game directory, app bundle, or game executable: {}",
            configured_path.display()
        )))
    }

    pub fn auto_detect() -> Option<Self> {
        if let Some(path) = std::env::var_os("LTK_LEAGUE_PATH") {
            if let Ok(install) = Self::resolve(PathBuf::from(path)) {
                return Some(install);
            }
        }

        #[cfg(target_os = "macos")]
        {
            for path in macos_common_paths() {
                if let Ok(install) = Self::resolve(path) {
                    return Some(install);
                }
            }

            if let Some(path) = detect_macos_running_process() {
                if let Ok(install) = Self::resolve(path) {
                    return Some(install);
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(exe_path) = ltk_mod_core::auto_detect_league_path() {
                if let Ok(install) = Self::resolve(exe_path.as_str()) {
                    return Some(install);
                }
            }
        }

        None
    }

    pub fn configured_root(&self) -> PathBuf {
        self.install_root.clone()
    }
}

fn canonical_or_original(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn resolve_windows(input: &Path) -> Option<LeagueInstall> {
    let mut candidates = Vec::new();
    if input.file_name().and_then(|name| name.to_str()) == Some(WINDOWS_GAME_EXE) {
        candidates.push(input.parent()?.to_path_buf());
    }
    candidates.push(input.to_path_buf());
    candidates.push(input.join("Game"));

    for game_dir in candidates {
        let game_executable = game_dir.join(WINDOWS_GAME_EXE);
        if !game_executable.is_file() {
            continue;
        }

        let game_dir = canonical_or_original(game_dir);
        let install_root = game_dir.parent()?.to_path_buf();
        return Some(LeagueInstall {
            configured_path: input.to_path_buf(),
            client_lockfile: install_root.join("lockfile"),
            install_root,
            game_executable: canonical_or_original(game_executable),
            game_dir,
            game_bundle: None,
            platform: LeaguePlatform::Windows,
        });
    }

    None
}

fn resolve_macos(input: &Path) -> Option<LeagueInstall> {
    let search_start = if input.is_file() {
        input.parent()?
    } else {
        input
    };

    let mut game_candidates = vec![
        search_start.to_path_buf(),
        search_start.join("Game"),
        search_start.join("Contents").join("LoL").join("Game"),
    ];
    game_candidates.extend(
        search_start
            .ancestors()
            .take(8)
            .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("Game"))
            .map(Path::to_path_buf),
    );
    game_candidates.extend(
        search_start
            .ancestors()
            .take(10)
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("app"))
            .map(|bundle| bundle.join("Contents").join("LoL").join("Game")),
    );

    for game_dir in game_candidates {
        let game_bundle = game_dir.join(MAC_GAME_APP);
        let game_executable = game_bundle
            .join("Contents")
            .join("MacOS")
            .join(MAC_GAME_EXE);
        if !game_executable.is_file() {
            continue;
        }

        let lol_root = game_dir.parent()?.to_path_buf();
        let outer_bundle = outer_macos_bundle(&lol_root).unwrap_or_else(|| lol_root.clone());
        return Some(LeagueInstall {
            configured_path: input.to_path_buf(),
            install_root: canonical_or_original(outer_bundle),
            game_dir: canonical_or_original(game_dir),
            client_lockfile: canonical_or_original(lol_root).join("lockfile"),
            game_executable: canonical_or_original(game_executable),
            game_bundle: Some(canonical_or_original(game_bundle)),
            platform: LeaguePlatform::MacOs,
        });
    }

    None
}

fn outer_macos_bundle(lol_root: &Path) -> Option<PathBuf> {
    if lol_root.file_name().and_then(|name| name.to_str()) != Some("LoL") {
        return None;
    }
    let contents = lol_root.parent()?;
    if contents.file_name().and_then(|name| name.to_str()) != Some("Contents") {
        return None;
    }
    let bundle = contents.parent()?;
    bundle
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("app"))?;
    Some(bundle.to_path_buf())
}

#[cfg(target_os = "macos")]
fn macos_common_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/Applications/League of Legends.app"),
        PathBuf::from("/Applications/LeagueofLegends.app"),
        PathBuf::from("/Users/Shared/Riot Games/League of Legends.app"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            PathBuf::from(home)
                .join("Applications")
                .join("League of Legends.app"),
        );
    }
    paths
}

#[cfg(target_os = "macos")]
fn detect_macos_running_process() -> Option<PathBuf> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.contains("LeagueClient.app/Contents/MacOS/LeagueClient")
                || line.contains("LeagueofLegends.app/Contents/MacOS/LeagueofLegends")
        })
        .find_map(|line| LeagueInstall::resolve(line).ok())
        .map(|install| install.install_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_windows_install(root: &Path) -> PathBuf {
        let game = root.join("Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(WINDOWS_GAME_EXE), b"fixture").unwrap();
        root.to_path_buf()
    }

    fn create_macos_install(root: &Path) -> PathBuf {
        let bundle = root.join("League of Legends.app");
        let game = bundle.join("Contents").join("LoL").join("Game");
        let executable = game
            .join(MAC_GAME_APP)
            .join("Contents")
            .join("MacOS")
            .join(MAC_GAME_EXE);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(executable, b"fixture").unwrap();
        bundle
    }

    #[test]
    fn resolves_windows_root_game_dir_and_executable() {
        let temp = tempfile::tempdir().unwrap();
        let root = create_windows_install(temp.path());
        let game = root.join("Game");
        let exe = game.join(WINDOWS_GAME_EXE);
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let canonical_game = std::fs::canonicalize(&game).unwrap();
        let canonical_exe = std::fs::canonicalize(&exe).unwrap();

        for input in [&root, &game, &exe] {
            let install = LeagueInstall::resolve(input).unwrap();
            assert_eq!(install.platform, LeaguePlatform::Windows);
            assert_eq!(install.install_root, canonical_root);
            assert_eq!(install.game_dir, canonical_game);
            assert_eq!(install.game_executable, canonical_exe);
            assert_eq!(install.client_lockfile, canonical_root.join("lockfile"));
        }
    }

    #[test]
    fn resolves_macos_bundle_lol_root_game_dir_and_nested_selection() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = create_macos_install(temp.path());
        let lol_root = bundle.join("Contents").join("LoL");
        let game = lol_root.join("Game");
        let nested_bundle = game.join(MAC_GAME_APP);
        let executable = nested_bundle
            .join("Contents")
            .join("MacOS")
            .join(MAC_GAME_EXE);
        let client_executable = bundle
            .join("Contents")
            .join("LoL")
            .join("LeagueClient.app")
            .join("Contents")
            .join("MacOS")
            .join("LeagueClient");
        std::fs::create_dir_all(client_executable.parent().unwrap()).unwrap();
        std::fs::write(&client_executable, b"fixture").unwrap();
        let canonical_bundle = std::fs::canonicalize(&bundle).unwrap();
        let canonical_lol_root = std::fs::canonicalize(&lol_root).unwrap();
        let canonical_game = std::fs::canonicalize(&game).unwrap();
        let canonical_nested_bundle = std::fs::canonicalize(&nested_bundle).unwrap();
        let canonical_executable = std::fs::canonicalize(&executable).unwrap();

        for input in [
            &bundle,
            &lol_root,
            &game,
            &nested_bundle,
            &executable,
            &client_executable,
        ] {
            let install = LeagueInstall::resolve(input).unwrap();
            assert_eq!(install.platform, LeaguePlatform::MacOs);
            assert_eq!(install.install_root, canonical_bundle);
            assert_eq!(install.game_dir, canonical_game);
            assert_eq!(install.game_executable, canonical_executable);
            assert_eq!(install.game_bundle.as_ref(), Some(&canonical_nested_bundle));
            assert_eq!(install.client_lockfile, canonical_lol_root.join("lockfile"));
        }
    }

    #[test]
    fn rejects_incomplete_layouts() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("Game")).unwrap();
        assert!(LeagueInstall::resolve(temp.path()).is_err());
    }
}
