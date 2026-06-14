//! Read-only utilities for resolving and inspecting a League game directory.

use crate::error::{AppError, AppResult};
use crate::state::Settings;
use std::path::{Path, PathBuf};

/// Resolve the game directory (the one containing `DATA`) from settings.
///
/// Users may configure either the install root (`…/League of Legends`) or the
/// `Game` subdirectory directly; both are accepted.
pub(crate) fn resolve_game_dir(settings: &Settings) -> AppResult<PathBuf> {
    let league_root = settings
        .league_path
        .clone()
        .ok_or_else(|| AppError::ValidationFailed("League path is not configured".to_string()))?;

    let game_dir = league_root.join("Game");
    if game_dir.exists() {
        return Ok(game_dir);
    }
    if league_root.join("DATA").exists() {
        return Ok(league_root);
    }

    Err(AppError::ValidationFailed(format!(
        "League path does not look like an install root or a Game directory: {}",
        league_root.display()
    )))
}

/// Enumerate every `.wad` / `.wad.client` filename under the game's `DATA` directory.
///
/// Returns lowercased, deduplicated filenames (not paths) sorted alphabetically.
/// The WAD filename space is effectively flat from the overlay's perspective —
/// `OverlayBuilder::with_blocked_wads` matches by filename only — so we discard
/// the directory part.
pub(crate) fn list_game_wads(game_dir: &Path) -> AppResult<Vec<String>> {
    let data_dir = game_dir.join("DATA");
    if !data_dir.exists() {
        return Err(AppError::ValidationFailed(format!(
            "Game DATA directory does not exist: {}",
            data_dir.display()
        )));
    }

    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![data_dir];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to read {}: {}", dir.display(), e);
                continue;
            }
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".wad") || lower.ends_with(".wad.client") {
                out.push(lower);
            }
        }
    }

    out.sort();
    out.dedup();
    Ok(out)
}

/// Read champion internal names (WAD stems, e.g. `"Aatrox"`, `"MonkeyKing"`)
/// from `{game_dir}/DATA/FINAL/Champions`. Returns an empty list (logged at
/// debug) when the directory can't be read — the resulting roster then matches
/// no champion and the coarse WAD footprint still applies.
pub(crate) fn read_champion_names(game_dir: &Path) -> Vec<String> {
    let champ_dir = game_dir.join("DATA").join("FINAL").join("Champions");
    let entries = match std::fs::read_dir(&champ_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(
                "Champion roster unavailable at {}: {}",
                champ_dir.display(),
                e
            );
            return Vec::new();
        }
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.to_ascii_lowercase()
                .ends_with(".wad.client")
                .then(|| name.split('.').next().unwrap_or(name.as_str()).to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[test]
    fn resolve_game_dir_no_league_path() {
        let settings = Settings::default();
        assert_matches!(
            resolve_game_dir(&settings),
            Err(AppError::ValidationFailed(_))
        );
    }

    #[test]
    fn resolve_game_dir_with_game_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Game")).unwrap();

        let settings = Settings {
            league_path: Some(dir.path().to_path_buf()),
            ..Settings::default()
        };
        let result = resolve_game_dir(&settings).unwrap();
        assert!(result.ends_with("Game"));
    }

    #[test]
    fn resolve_game_dir_with_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DATA")).unwrap();

        let settings = Settings {
            league_path: Some(dir.path().to_path_buf()),
            ..Settings::default()
        };
        let result = resolve_game_dir(&settings).unwrap();
        assert_eq!(result, dir.path().to_path_buf());
    }

    #[test]
    fn resolve_game_dir_neither_dir() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            league_path: Some(dir.path().to_path_buf()),
            ..Settings::default()
        };
        assert_matches!(
            resolve_game_dir(&settings),
            Err(AppError::ValidationFailed(_))
        );
    }

    #[test]
    fn list_game_wads_finds_nested_wads_and_lowercases() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("DATA");
        let final_dir = data.join("FINAL").join("Champions");
        std::fs::create_dir_all(&final_dir).unwrap();
        std::fs::write(final_dir.join("Aatrox.wad.client"), b"").unwrap();
        std::fs::write(final_dir.join("Ahri.wad.client"), b"").unwrap();
        std::fs::write(data.join("Shared.wad"), b"").unwrap();
        std::fs::write(final_dir.join("not-a-wad.txt"), b"").unwrap();

        let wads = list_game_wads(dir.path()).unwrap();
        assert!(wads.contains(&"aatrox.wad.client".to_string()));
        assert!(wads.contains(&"ahri.wad.client".to_string()));
        assert!(wads.contains(&"shared.wad".to_string()));
        assert_eq!(wads.len(), 3);
    }

    #[test]
    fn list_game_wads_errors_when_data_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_matches!(
            list_game_wads(dir.path()),
            Err(AppError::ValidationFailed(_))
        );
    }

    #[test]
    fn read_champion_names_reads_wad_stems() {
        let dir = tempfile::tempdir().unwrap();
        let champ_dir = dir.path().join("DATA").join("FINAL").join("Champions");
        std::fs::create_dir_all(&champ_dir).unwrap();
        std::fs::write(champ_dir.join("Aatrox.wad.client"), b"").unwrap();
        std::fs::write(champ_dir.join("MonkeyKing.wad.client"), b"").unwrap();
        std::fs::write(champ_dir.join("readme.txt"), b"").unwrap();

        let mut names = read_champion_names(dir.path());
        names.sort();
        assert_eq!(names, vec!["Aatrox", "MonkeyKing"]);
    }

    #[test]
    fn read_champion_names_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_champion_names(dir.path()).is_empty());
    }
}
