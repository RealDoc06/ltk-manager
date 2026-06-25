use crate::error::{AppError, AppResult, Utf8PathExt};
use crate::mods::{ModLibrary, WadReportState};
use crate::state::{Settings, WadBlocklistEntry};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};
use tauri::{Emitter, Manager};

pub mod linked_bins;

const SCRIPTS_WAD: &str = "scripts.wad.client";
const TFT_WAD: &str = "map22.wad.client";
#[cfg(target_os = "macos")]
const WAD_V3_SIGNATURE_OFFSET: u64 = 4;
#[cfg(target_os = "macos")]
const WAD_V3_SIGNATURE_SIZE: usize = 256;
#[cfg(target_os = "macos")]
const WAD_V3_CHECKSUM_OFFSET: u64 = 4 + 256;

const MACOS_PLATFORM_WADS: &[&str] = &[
    "bootstrap.macos.wad.client",
    "shadercache.metal.wad.client",
    "shaders.wad.client",
];

#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OverlayStage {
    Indexing,
    Collecting,
    Patching,
    Strings,
    Complete,
}

/// Progress event emitted during overlay building.
#[derive(Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OverlayProgress {
    pub stage: OverlayStage,
    pub current_file: Option<String>,
    pub current: u32,
    pub total: u32,
}

impl ModLibrary {
    /// Ensure the overlay exists and is up-to-date for the current enabled mod set.
    ///
    /// Returns the overlay root directory (the prefix passed to the legacy patcher).
    ///
    /// Workshop project paths (if any) are loaded via `FsModContent` and prepended
    /// to the enabled mod list so they take highest priority.
    pub fn ensure_overlay(
        &self,
        settings: &Settings,
        workshop_project_paths: &[PathBuf],
    ) -> AppResult<PathBuf> {
        let storage_dir = self.storage_dir(settings)?;
        let game_dir = crate::utils::game::resolve_game_dir(settings)?;
        let (profile_slug, enabled_mods) = self.get_enabled_mods_for_overlay(settings)?;

        let profile_dir = storage_dir.join("profiles").join(profile_slug.as_str());
        let overlay_root = profile_dir.join("overlay");

        tracing::info!("Overlay: storage_dir={}", storage_dir.display());
        tracing::info!("Overlay: profile_slug={}", profile_slug);
        tracing::info!("Overlay: overlay_root={}", overlay_root.display());
        tracing::info!("Overlay: game_dir={}", game_dir.display());

        let enabled_ids = enabled_mods
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        tracing::info!(
            "Overlay: enabled_mods={} ids=[{}]",
            enabled_ids.len(),
            enabled_ids.join(", ")
        );

        let utf8_game_dir = game_dir.clone().try_into_utf8("game directory")?;
        let utf8_overlay_root = overlay_root.clone().try_into_utf8("overlay root")?;
        let utf8_state_dir = profile_dir.try_into_utf8("profile directory")?;

        let available_wads = crate::utils::game::list_game_wads(&game_dir).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to enumerate game WADs for regex expansion: {}; \
                 regex blocklist entries will match nothing",
                e
            );
            Vec::new()
        });
        let blocked_wads = resolve_blocked_wads(settings, &available_wads);
        tracing::info!("Overlay: blocked_wads count={}", blocked_wads.len());

        Self::clean_corrupt_overlay_state(&utf8_state_dir);

        let app_handle_clone = self.app_handle().clone();
        let mut builder =
            ltk_overlay::OverlayBuilder::new(utf8_game_dir, utf8_overlay_root, utf8_state_dir)
                .with_blocked_wads(blocked_wads.clone())
                .with_progress(move |progress| {
                    let stage = match progress.stage {
                        ltk_overlay::OverlayStage::Indexing => OverlayStage::Indexing,
                        ltk_overlay::OverlayStage::CollectingOverrides => OverlayStage::Collecting,
                        ltk_overlay::OverlayStage::PatchingWad => OverlayStage::Patching,
                        ltk_overlay::OverlayStage::ApplyingStringOverrides => OverlayStage::Strings,
                        ltk_overlay::OverlayStage::Complete => OverlayStage::Complete,
                    };
                    let _ = app_handle_clone.emit(
                        "overlay-progress",
                        OverlayProgress {
                            stage,
                            current_file: progress.current_file,
                            current: progress.current,
                            total: progress.total,
                        },
                    );
                });

        let mut all_mods = Vec::new();
        for project_path in workshop_project_paths {
            let utf8_path = project_path
                .clone()
                .try_into_utf8("workshop project path")?;
            let dir_name = project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            let id = format!("workshop:{}", dir_name);
            tracing::info!("Adding workshop project: id={}, path={}", id, utf8_path);
            all_mods.push(ltk_overlay::EnabledMod {
                id,
                content: Box::new(ltk_overlay::FsModContent::new(utf8_path)),
                enabled_layers: None,
            });
        }
        all_mods.extend(enabled_mods);
        builder.set_enabled_mods(all_mods);

        builder
            .build()
            .map_err(|e| AppError::Other(format!("Overlay build failed: {}", e)))?;

        #[cfg(target_os = "macos")]
        prepare_macos_overlay_wads(&overlay_root, &game_dir, &blocked_wads)?;

        // Capture per-mod WAD reports for the library badge UI. Failure to
        // persist must not fail the patch — log and continue.
        //
        // Note: `OverlayBuilder::build()` emits its own `Complete` progress event
        // *before* returning, so the frontend may see that event before the reports
        // are persisted. We emit a dedicated `wad-reports-updated` event after
        // persisting so the frontend knows the cache is ready to query.
        let reports = builder.take_mod_wad_reports();
        if !reports.is_empty() {
            if let Some(state) = self.app_handle().try_state::<WadReportState>() {
                if let Err(e) = state.record_reports(reports) {
                    tracing::warn!("Failed to persist per-mod WAD reports: {}", e);
                } else {
                    let _ = self.app_handle().emit("wad-reports-updated", ());
                }
            }
        }

        Ok(overlay_root)
    }

    /// Scan `state_dir` for top-level JSON files that are empty or contain invalid
    /// JSON and remove them so `ltk_overlay` does not fail to parse stale/corrupt
    /// state files written by a previous run that was interrupted mid-write.
    fn clean_corrupt_overlay_state(state_dir: &camino::Utf8Path) {
        let entries = match std::fs::read_dir(state_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let contents = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if contents.trim().is_empty()
                || serde_json::from_str::<serde_json::Value>(&contents).is_err()
            {
                tracing::warn!(
                    "Removing corrupt overlay state file before build: {}",
                    path.display()
                );
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn prepare_macos_overlay_wads(
    overlay_root: &Path,
    game_dir: &Path,
    blocked_wads: &[String],
) -> AppResult<()> {
    let passthroughs = create_blocked_wad_passthroughs(overlay_root, game_dir, blocked_wads)?;
    let data_dir = overlay_root.join("DATA");
    if !data_dir.exists() {
        return Ok(());
    }

    let mut restored = 0;
    let mut stripped = 0;
    let mut reverted = 0;
    let mut repacked = 0;
    let mut repaired = 0;
    for entry in walkdir::WalkDir::new(&data_dir).follow_links(false) {
        let entry = entry.map_err(|error| {
            AppError::Other(format!("Failed to scan macOS overlay WADs: {}", error))
        })?;
        if !entry.file_type().is_file() || entry.file_type().is_symlink() {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if file_name.ends_with(".wad") || file_name.ends_with(".wad.client") {
            let source_path = game_dir.join("DATA").join(
                entry
                    .path()
                    .strip_prefix(&data_dir)
                    .map_err(|error| AppError::Other(error.to_string()))?,
            );
            // Revert any cross-WAD overrides that clobbered a subchunk entry in
            // this WAD with a standalone (non-subchunked) chunk. ltk_overlay's
            // cross-WAD distribution doesn't account for the same path_hash
            // being subchunked in one WAD but standalone in another — see the
            // Aatrox crash root cause. Always run this *before* canonicalize so
            // canonicalize sees the restored subchunk and treats it correctly.
            if restore_subchunk_overrides(entry.path(), &source_path)? {
                restored += 1;
            }
            // Strip oversized new audio (Wwise .bnk / .wpk) entries that the
            // macOS game's audio engine can't ingest. Vayne ships a 36 MB
            // BKHD bank as a brand-new chunk that crashes the game right after
            // the loading screen completes. The mod still applies textures/VFX,
            // just without custom voice lines.
            let stripped_hashes = strip_oversized_audio_chunks(entry.path(), &source_path)?;
            if !stripped_hashes.is_empty() {
                stripped += 1;
                // The BIN/PTCH files that the mod ships as overrides still
                // reference the just-stripped audio paths by hash. When the game
                // looks them up it gets `AudioManager: Failed to load Bank for
                // Wwise (...)` and then crashes shortly after. Revert any
                // mod-overridden BIN that points at a stripped chunk so the
                // game loads its original audio config instead.
                if revert_audio_referring_overrides(entry.path(), &source_path, &stripped_hashes)? {
                    reverted += 1;
                }
            }
            if canonicalize_macos_wad(entry.path(), &source_path)? {
                repacked += 1;
            } else if repair_macos_wad_header(entry.path(), &source_path)? {
                repaired += 1;
            }
        }
    }

    tracing::info!(
        "Overlay: restored subchunks in {} WAD(s), stripped oversized audio in {} WAD(s), reverted dangling-audio BIN overrides in {} WAD(s), canonicalized {} macOS WAD(s), repaired headers for {} WAD(s), linked {} blocked WAD passthrough(s)",
        restored,
        stripped,
        reverted,
        repacked,
        repaired,
        passthroughs
    );
    Ok(())
}

/// Maximum size (in bytes) for a mod-added Wwise audio chunk. Banks above this
/// threshold crash the macOS game's Wwise loader on first playback. Riot's own
/// audio banks live well under this limit; oversized mod banks are dropped from
/// the overlay so the visual portion of the mod still applies.
#[cfg(target_os = "macos")]
const MAX_MACOS_AUDIO_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Drop new (not-in-source) Wwise `.bnk`/`.wpk` entries whose compressed size
/// exceeds [`MAX_MACOS_AUDIO_CHUNK_BYTES`]. Returns the set of path hashes
/// that were stripped, empty if nothing matched.
#[cfg(target_os = "macos")]
fn strip_oversized_audio_chunks(path: &Path, source_path: &Path) -> AppResult<HashSet<u64>> {
    use byteorder::{WriteBytesExt as _, LE};
    use ltk_file::LeagueFileKind;
    use ltk_wad::WadChunk;

    let mut overlay_wad = ltk_wad::Wad::mount(File::open(path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read overlay WAD {}: {}",
            path.display(),
            error
        ))
    })?;
    let overlay_chunks = overlay_wad.chunks().clone();

    let source_path_hashes: HashSet<u64> = if source_path.exists() {
        let source_wad = ltk_wad::Wad::mount(File::open(source_path)?).map_err(|error| {
            AppError::Other(format!(
                "Failed to read source WAD {}: {}",
                source_path.display(),
                error
            ))
        })?;
        source_wad.chunks().iter().map(|c| c.path_hash).collect()
    } else {
        HashSet::new()
    };

    let mut drop_hashes: HashSet<u64> = HashSet::new();
    for chunk in &overlay_chunks {
        if chunk.compressed_size <= MAX_MACOS_AUDIO_CHUNK_BYTES {
            continue;
        }
        if source_path_hashes.contains(&chunk.path_hash) {
            continue;
        }
        // Peek at the first few bytes to confirm it's audio. We only want to
        // drop entries that the audio engine will try to ingest.
        let raw = overlay_wad.load_chunk_raw(chunk).map_err(|error| {
            AppError::Other(format!(
                "Failed to read overlay chunk {:016x} from {}: {}",
                chunk.path_hash,
                path.display(),
                error
            ))
        })?;
        let kind = LeagueFileKind::identify_from_bytes(&raw);
        if matches!(
            kind,
            LeagueFileKind::WwiseBank | LeagueFileKind::WwisePackage
        ) {
            drop_hashes.insert(chunk.path_hash);
        }
    }

    if drop_hashes.is_empty() {
        return Ok(HashSet::new());
    }

    tracing::info!(
        "Overlay: stripping {} oversized audio entry/entries from {}",
        drop_hashes.len(),
        path.display()
    );

    let mut signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    let mut source = File::open(path)?;
    source.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    source.read_exact(&mut signature)?;

    let kept: Vec<&WadChunk> = overlay_chunks
        .iter()
        .filter(|chunk| !drop_hashes.contains(&chunk.path_hash))
        .collect();

    let temporary_path = path.with_extension("ltk-strip-tmp");
    let result = (|| -> AppResult<()> {
        let mut writer = BufWriter::new(File::create(&temporary_path)?);
        let version = [b'R', b'W', 3, 4];
        writer.write_all(&version)?;
        writer.write_all(&signature)?;
        writer.write_u64::<LE>(0)?;
        writer.write_u32::<LE>(kept.len() as u32)?;
        let toc_offset = writer.stream_position()?;
        writer.write_all(&vec![0_u8; kept.len() * 32])?;

        let mut final_chunks: Vec<WadChunk> = Vec::with_capacity(kept.len());
        for chunk in &kept {
            let raw = overlay_wad.load_chunk_raw(chunk).map_err(|error| {
                AppError::Other(format!(
                    "Failed to read overlay chunk {:016x} from {}: {}",
                    chunk.path_hash,
                    path.display(),
                    error
                ))
            })?;
            let data_offset = writer.stream_position()? as usize;
            writer.write_all(&raw)?;
            final_chunks.push(WadChunk {
                path_hash: chunk.path_hash,
                data_offset,
                compressed_size: raw.len(),
                ..**chunk
            });
        }

        writer.seek(SeekFrom::Start(toc_offset))?;
        for chunk in &final_chunks {
            chunk.write_v3_4(&mut writer).map_err(|error| {
                AppError::Other(format!(
                    "Failed to write chunk table for {}: {}",
                    path.display(),
                    error
                ))
            })?;
        }
        writer.flush()?;
        drop(writer);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result?;
    Ok(drop_hashes)
}

/// After stripping audio chunks, find any override BIN file (PROP/PTCH) whose
/// path references a stripped chunk's hash and revert it to the source. This
/// stops the game from issuing dead lookups like `Failed to load Bank for
/// Wwise (Vayne_SFX_audio.bnk)` that crash the audio engine downstream when
/// it tries to play an event from the missing bank. Returns `true` if any
/// override was reverted.
///
/// We compute `xxh64(lowercase(path), 0)` for every printable `.bnk`/`.wpk`
/// path string inside each Zstd-compressed override and compare against the
/// stripped set. That's the same hashing convention League uses internally
/// for WAD path lookups.
#[cfg(target_os = "macos")]
fn revert_audio_referring_overrides(
    path: &Path,
    source_path: &Path,
    stripped_hashes: &HashSet<u64>,
) -> AppResult<bool> {
    use byteorder::{WriteBytesExt as _, LE};
    use ltk_wad::{WadChunk, WadChunkCompression};
    use xxhash_rust::xxh64::xxh64;

    if stripped_hashes.is_empty() || !source_path.exists() {
        return Ok(false);
    }

    let mut overlay_wad = ltk_wad::Wad::mount(File::open(path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read overlay WAD {}: {}",
            path.display(),
            error
        ))
    })?;
    let mut source_wad = ltk_wad::Wad::mount(File::open(source_path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read source WAD {}: {}",
            source_path.display(),
            error
        ))
    })?;

    let overlay_chunks = overlay_wad.chunks().clone();
    let source_chunks = source_wad.chunks().clone();

    let mut to_revert: HashMap<u64, WadChunk> = HashMap::new();
    for chunk in &overlay_chunks {
        let Some(source_chunk) = source_chunks.get(chunk.path_hash) else {
            continue;
        };
        if source_chunk.checksum == chunk.checksum {
            continue; // Not an override.
        }
        if chunk.compression_type != WadChunkCompression::Zstd {
            continue; // BINs are always Zstd-compressed in practice.
        }
        let decompressed = match overlay_wad.load_chunk_decompressed(chunk) {
            Ok(data) => data,
            Err(_) => continue,
        };
        if !(decompressed.starts_with(b"PROP") || decompressed.starts_with(b"PTCH")) {
            continue; // Only inspect property-bin chunks.
        }
        if bin_references_stripped_audio(&decompressed, stripped_hashes, &xxh64) {
            to_revert.insert(chunk.path_hash, *source_chunk);
        }
    }

    if to_revert.is_empty() {
        return Ok(false);
    }

    tracing::info!(
        "Overlay: reverting {} BIN override(s) referencing stripped audio in {}",
        to_revert.len(),
        path.display()
    );

    let mut signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    let mut source = File::open(source_path)?;
    source.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    source.read_exact(&mut signature)?;

    let temporary_path = path.with_extension("ltk-bin-revert-tmp");
    let result = (|| -> AppResult<()> {
        let mut writer = BufWriter::new(File::create(&temporary_path)?);
        let version = [b'R', b'W', 3, 4];
        writer.write_all(&version)?;
        writer.write_all(&signature)?;
        writer.write_u64::<LE>(0)?;
        writer.write_u32::<LE>(overlay_chunks.len() as u32)?;
        let toc_offset = writer.stream_position()?;
        writer.write_all(&vec![0_u8; overlay_chunks.len() * 32])?;

        let mut final_chunks: Vec<WadChunk> = Vec::with_capacity(overlay_chunks.len());
        for chunk in &overlay_chunks {
            let final_chunk = if let Some(source_chunk) = to_revert.get(&chunk.path_hash) {
                let raw = source_wad.load_chunk_raw(source_chunk).map_err(|error| {
                    AppError::Other(format!(
                        "Failed to read source chunk {:016x} from {}: {}",
                        chunk.path_hash,
                        source_path.display(),
                        error
                    ))
                })?;
                let data_offset = writer.stream_position()? as usize;
                writer.write_all(&raw)?;
                WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset,
                    compressed_size: raw.len(),
                    uncompressed_size: source_chunk.uncompressed_size,
                    compression_type: source_chunk.compression_type,
                    is_duplicated: false,
                    frame_count: source_chunk.frame_count,
                    start_frame: source_chunk.start_frame,
                    checksum: source_chunk.checksum,
                }
            } else {
                let raw = overlay_wad.load_chunk_raw(chunk).map_err(|error| {
                    AppError::Other(format!(
                        "Failed to read overlay chunk {:016x} from {}: {}",
                        chunk.path_hash,
                        path.display(),
                        error
                    ))
                })?;
                let data_offset = writer.stream_position()? as usize;
                writer.write_all(&raw)?;
                WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset,
                    compressed_size: raw.len(),
                    ..*chunk
                }
            };
            final_chunks.push(final_chunk);
        }

        writer.seek(SeekFrom::Start(toc_offset))?;
        for chunk in &final_chunks {
            chunk.write_v3_4(&mut writer).map_err(|error| {
                AppError::Other(format!(
                    "Failed to write chunk table for {}: {}",
                    path.display(),
                    error
                ))
            })?;
        }
        writer.flush()?;
        drop(writer);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result?;
    Ok(true)
}

/// Scan a decompressed PROP/PTCH BIN for `.bnk` / `.wpk` path strings; return
/// `true` as soon as one of them hashes to any entry in `stripped_hashes`.
#[cfg(target_os = "macos")]
fn bin_references_stripped_audio(
    data: &[u8],
    stripped_hashes: &HashSet<u64>,
    hash_fn: &impl Fn(&[u8], u64) -> u64,
) -> bool {
    let mut i = 0;
    while i + 4 <= data.len() {
        let window = &data[i..i + 4];
        if window == b".bnk" || window == b".wpk" {
            // Walk backwards over the path string. Wwise paths are URL-safe
            // ASCII: letters, digits, `/`, `.`, `_`, `-`.
            let mut start = i;
            while start > 0 {
                let c = data[start - 1];
                let is_path_char =
                    c.is_ascii_alphanumeric() || c == b'/' || c == b'.' || c == b'_' || c == b'-';
                if !is_path_char {
                    break;
                }
                start -= 1;
            }
            let path = &data[start..i + 4];
            if path.len() >= 5 {
                let lower: Vec<u8> = path.iter().map(|c| c.to_ascii_lowercase()).collect();
                if stripped_hashes.contains(&hash_fn(&lower, 0)) {
                    return true;
                }
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    false
}

/// Walk the overlay WAD and detect entries whose original (source) WAD has
/// subchunked metadata (frame_count > 1 or start_frame > 0 on a ZstdMulti
/// chunk) that the overlay no longer carries — i.e. a cross-WAD mod override
/// landed on top of a subchunk entry. For those entries, copy the original
/// chunk bytes from `source_path` into a new overlay WAD that keeps every
/// other entry untouched. Returns `true` if anything was restored.
#[cfg(target_os = "macos")]
fn restore_subchunk_overrides(path: &Path, source_path: &Path) -> AppResult<bool> {
    use byteorder::{WriteBytesExt as _, LE};
    use ltk_wad::{WadChunk, WadChunkCompression};

    if !source_path.exists() {
        return Ok(false);
    }

    let mut overlay_wad = ltk_wad::Wad::mount(File::open(path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read overlay WAD {}: {}",
            path.display(),
            error
        ))
    })?;
    let mut source_wad = ltk_wad::Wad::mount(File::open(source_path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read source WAD {}: {}",
            source_path.display(),
            error
        ))
    })?;

    let overlay_chunks = overlay_wad.chunks().clone();
    let source_chunks = source_wad.chunks().clone();

    // Walk overlay chunks; if the source has the same path_hash and it's a
    // subchunk over there but isn't here, plan to restore.
    let mut to_restore: HashMap<u64, WadChunk> = HashMap::new();
    for chunk in &overlay_chunks {
        let Some(source_chunk) = source_chunks.get(chunk.path_hash) else {
            continue;
        };
        let source_is_subchunk = source_chunk.compression_type == WadChunkCompression::ZstdMulti
            && (source_chunk.frame_count > 1 || source_chunk.start_frame != 0);
        if !source_is_subchunk {
            continue;
        }
        let overlay_still_matches = chunk.compression_type == source_chunk.compression_type
            && chunk.frame_count == source_chunk.frame_count
            && chunk.start_frame == source_chunk.start_frame
            && chunk.uncompressed_size == source_chunk.uncompressed_size;
        if overlay_still_matches {
            continue;
        }
        to_restore.insert(chunk.path_hash, *source_chunk);
    }

    if to_restore.is_empty() {
        return Ok(false);
    }

    tracing::info!(
        "Overlay: restoring {} subchunk entry/entries in {} from {}",
        to_restore.len(),
        path.display(),
        source_path.display()
    );

    let mut signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    let mut source = File::open(source_path)?;
    source.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    source.read_exact(&mut signature)?;

    let temporary_path = path.with_extension("ltk-restore-tmp");
    let result = (|| -> AppResult<()> {
        let mut writer = BufWriter::new(File::create(&temporary_path)?);
        let version = [b'R', b'W', 3, 4];
        writer.write_all(&version)?;
        writer.write_all(&signature)?;
        writer.write_u64::<LE>(0)?;
        writer.write_u32::<LE>(overlay_chunks.len() as u32)?;
        let toc_offset = writer.stream_position()?;
        writer.write_all(&vec![0_u8; overlay_chunks.len() * 32])?;

        let mut final_chunks: Vec<WadChunk> = Vec::with_capacity(overlay_chunks.len());
        for chunk in &overlay_chunks {
            let final_chunk = if let Some(source_chunk) = to_restore.get(&chunk.path_hash) {
                // Copy bytes straight from the source WAD using the source
                // chunk's compressed_size/data_offset; rewrite the TOC entry
                // with the source's subchunk metadata.
                let raw = source_wad.load_chunk_raw(source_chunk).map_err(|error| {
                    AppError::Other(format!(
                        "Failed to read source chunk {:016x} from {}: {}",
                        chunk.path_hash,
                        source_path.display(),
                        error
                    ))
                })?;
                let data_offset = writer.stream_position()? as usize;
                writer.write_all(&raw)?;
                WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset,
                    compressed_size: raw.len(),
                    uncompressed_size: source_chunk.uncompressed_size,
                    compression_type: source_chunk.compression_type,
                    is_duplicated: false,
                    frame_count: source_chunk.frame_count,
                    start_frame: source_chunk.start_frame,
                    checksum: source_chunk.checksum,
                }
            } else {
                let raw = overlay_wad.load_chunk_raw(chunk).map_err(|error| {
                    AppError::Other(format!(
                        "Failed to read overlay chunk {:016x} from {}: {}",
                        chunk.path_hash,
                        path.display(),
                        error
                    ))
                })?;
                let data_offset = writer.stream_position()? as usize;
                writer.write_all(&raw)?;
                WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset,
                    compressed_size: raw.len(),
                    ..*chunk
                }
            };
            final_chunks.push(final_chunk);
        }

        writer.seek(SeekFrom::Start(toc_offset))?;
        for chunk in &final_chunks {
            chunk.write_v3_4(&mut writer).map_err(|error| {
                AppError::Other(format!(
                    "Failed to write chunk table for {}: {}",
                    path.display(),
                    error
                ))
            })?;
        }
        writer.flush()?;
        drop(writer);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn create_blocked_wad_passthroughs(
    overlay_root: &Path,
    game_dir: &Path,
    blocked_wads: &[String],
) -> AppResult<usize> {
    let blocked: HashSet<String> = blocked_wads
        .iter()
        .map(|wad| wad.to_ascii_lowercase())
        .collect();
    if blocked.is_empty() {
        return Ok(0);
    }

    let game_data_dir = game_dir.join("DATA");
    let overlay_data_dir = overlay_root.join("DATA");
    if !game_data_dir.exists() {
        return Ok(0);
    }

    let mut linked = 0;
    for entry in walkdir::WalkDir::new(&game_data_dir).follow_links(false) {
        let entry = entry.map_err(|error| {
            AppError::Other(format!("Failed to scan blocked macOS WADs: {}", error))
        })?;
        if !entry.file_type().is_file() {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !blocked.contains(&file_name) {
            continue;
        }

        let relative_path = entry
            .path()
            .strip_prefix(&game_data_dir)
            .map_err(|error| AppError::Other(error.to_string()))?;
        let target_path = overlay_data_dir.join(relative_path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let replace = match fs::read_link(&target_path) {
            Ok(existing) => existing != entry.path(),
            Err(_) => target_path.exists(),
        };
        if replace {
            if target_path.is_dir() {
                fs::remove_dir_all(&target_path)?;
            } else {
                fs::remove_file(&target_path)?;
            }
        }
        if replace || !target_path.exists() {
            std::os::unix::fs::symlink(entry.path(), &target_path)?;
            linked += 1;
        }
    }

    Ok(linked)
}

#[cfg(target_os = "macos")]
fn canonicalize_macos_wad(path: &Path, source_path: &Path) -> AppResult<bool> {
    use byteorder::{WriteBytesExt as _, LE};
    use ltk_wad::{WadChunk, WadChunkCompression};
    use xxhash_rust::xxh3::{xxh3_64, Xxh3};

    let mut wad = ltk_wad::Wad::mount(File::open(path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read overlay WAD {}: {}",
            path.display(),
            error
        ))
    })?;
    let chunks = wad.chunks().clone();
    let mut seen_checksums = HashMap::new();
    let needs_repack = chunks.iter().any(|chunk| {
        chunk.compression_type == WadChunkCompression::ZstdMulti
            || seen_checksums
                .insert(chunk.checksum, chunk.data_offset)
                .is_some_and(|offset| offset != chunk.data_offset)
    });
    if !needs_repack {
        return Ok(false);
    }

    let mut signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    let mut source = File::open(source_path)?;
    source.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    source.read_exact(&mut signature)?;

    let temporary_path = path.with_extension("ltk-tmp");
    let result = (|| -> AppResult<()> {
        let mut writer = BufWriter::new(File::create(&temporary_path)?);
        let version = [b'R', b'W', 3, 4];
        writer.write_all(&version)?;
        writer.write_all(&signature)?;
        writer.write_u64::<LE>(0)?;
        writer.write_u32::<LE>(chunks.len() as u32)?;
        let toc_offset = writer.stream_position()?;
        writer.write_all(&vec![0_u8; chunks.len() * 32])?;

        let mut locations = HashMap::<u64, WadChunk>::new();
        let mut final_chunks = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            // A ZstdMulti chunk with frame_count > 1 or a non-zero start_frame is
            // a *subchunk* inside a shared multi-frame zstd stream — multiple TOC
            // entries point at the same compressed bytes but each reads a
            // different frame range. Decompressing and re-encoding as Zstd here
            // (the original codex path) collapses every subchunk to the data of
            // frame 0, which is exactly what crashed Aatrox/Vayne: their WADs
            // ship hundreds of these (audio, scripts, vfx). Pass these through
            // raw and let the dedup below merge identical streams while
            // preserving each entry's own subchunk metadata.
            let is_subchunked = chunk.compression_type == WadChunkCompression::ZstdMulti
                && (chunk.frame_count > 1 || chunk.start_frame != 0);
            let (raw, compression_type, uncompressed_size, frame_count, start_frame, checksum) =
                if chunk.compression_type == WadChunkCompression::ZstdMulti && !is_subchunked {
                    let decompressed = wad.load_chunk_decompressed(chunk).map_err(|error| {
                        AppError::Other(format!(
                            "Failed to decompress multi-frame chunk {:016x} in {}: {}",
                            chunk.path_hash,
                            path.display(),
                            error
                        ))
                    })?;
                    let compressed = zstd::stream::encode_all(&decompressed[..], 3)?;
                    let checksum = xxh3_64(&compressed);
                    (
                        compressed,
                        WadChunkCompression::Zstd,
                        decompressed.len(),
                        0,
                        0,
                        checksum,
                    )
                } else {
                    (
                        wad.load_chunk_raw(chunk)
                            .map_err(|error| {
                                AppError::Other(format!(
                                    "Failed to read chunk {:016x} in {}: {}",
                                    chunk.path_hash,
                                    path.display(),
                                    error
                                ))
                            })?
                            .into_vec(),
                        chunk.compression_type,
                        chunk.uncompressed_size,
                        chunk.frame_count,
                        chunk.start_frame,
                        chunk.checksum,
                    )
                };

            let final_chunk = if let Some(existing) = locations.get(&checksum) {
                // Share the same on-disk bytes with an earlier entry, but keep
                // *this* chunk's subchunk metadata (frame_count, start_frame,
                // uncompressed_size). Without this, subchunks all collapse to
                // the first entry's frame and the game reads the wrong slice.
                WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset: existing.data_offset,
                    compressed_size: existing.compressed_size,
                    uncompressed_size,
                    compression_type,
                    is_duplicated: false,
                    frame_count,
                    start_frame,
                    checksum: existing.checksum,
                }
            } else {
                let data_offset = writer.stream_position()? as usize;
                writer.write_all(&raw)?;
                let final_chunk = WadChunk {
                    path_hash: chunk.path_hash,
                    data_offset,
                    compressed_size: raw.len(),
                    uncompressed_size,
                    compression_type,
                    is_duplicated: false,
                    frame_count,
                    start_frame,
                    checksum,
                };
                locations.insert(checksum, final_chunk);
                final_chunk
            };
            final_chunks.push(final_chunk);
        }

        writer.seek(SeekFrom::Start(toc_offset))?;
        for chunk in &final_chunks {
            chunk.write_v3_4(&mut writer).map_err(|error| {
                AppError::Other(format!(
                    "Failed to write chunk table for {}: {}",
                    path.display(),
                    error
                ))
            })?;
        }

        let mut hasher = Xxh3::new();
        hasher.update(&version);
        for chunk in &final_chunks {
            hasher.update(&chunk.path_hash.to_le_bytes());
            hasher.update(&chunk.checksum.to_le_bytes());
        }
        writer.seek(SeekFrom::Start(WAD_V3_CHECKSUM_OFFSET))?;
        writer.write_u64::<LE>(hasher.digest())?;
        writer.flush()?;
        drop(writer);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn repair_macos_wad_header(path: &Path, source_path: &Path) -> AppResult<bool> {
    use byteorder::{ReadBytesExt as _, WriteBytesExt as _, LE};
    use xxhash_rust::xxh3::Xxh3;

    let mut version = [0_u8; 4];
    File::open(path)?.read_exact(&mut version)?;
    if version[0..2] != [b'R', b'W'] || version[2] != 3 {
        return Ok(false);
    }

    let wad = ltk_wad::Wad::mount(File::open(path)?).map_err(|error| {
        AppError::Other(format!(
            "Failed to read overlay WAD {}: {}",
            path.display(),
            error
        ))
    })?;
    let mut hasher = Xxh3::new();
    hasher.update(&version);
    for chunk in wad.chunks() {
        hasher.update(&chunk.path_hash.to_le_bytes());
        hasher.update(&chunk.checksum.to_le_bytes());
    }
    let checksum = hasher.digest();

    let mut signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    let mut source = File::open(source_path)?;
    source.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    source.read_exact(&mut signature)?;

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
    let mut current_signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
    file.read_exact(&mut current_signature)?;
    file.seek(SeekFrom::Start(WAD_V3_CHECKSUM_OFFSET))?;
    let current_checksum = file.read_u64::<LE>()?;
    if current_signature == signature && current_checksum == checksum {
        return Ok(false);
    }

    if current_signature != signature {
        file.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))?;
        file.write_all(&signature)?;
    }
    file.seek(SeekFrom::Start(WAD_V3_CHECKSUM_OFFSET))?;
    file.write_u64::<LE>(checksum)?;
    Ok(true)
}

/// Resolve the user's blocklist settings into a concrete, deduped list of WAD
/// filenames to hand to `ltk_overlay::OverlayBuilder::with_blocked_wads`.
///
/// - `Exact` entries are lowercased and passed through as-is.
/// - `Regex` entries are compiled case-insensitively and expanded against
///   `available_wads`; invalid patterns are logged and skipped so one bad entry
///   can't break the whole patch.
/// - `block_scripts_wad` and `!patch_tft` add their respective WADs.
///
/// `available_wads` should come from `crate::utils::game::list_game_wads`; pass an empty slice if
/// enumeration failed (regex entries then match nothing).
pub(crate) fn resolve_blocked_wads(settings: &Settings, available_wads: &[String]) -> Vec<String> {
    let mut blocked: Vec<String> = Vec::new();

    for entry in &settings.wad_blocklist {
        match entry {
            WadBlocklistEntry::Exact { value } => {
                blocked.push(value.to_lowercase());
            }
            WadBlocklistEntry::Regex { value } => {
                match regex::Regex::new(&format!("(?i){}", value)) {
                    Ok(re) => {
                        for wad in available_wads {
                            if re.is_match(wad) {
                                blocked.push(wad.clone());
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Invalid regex in wad_blocklist {:?}: {}", value, e);
                    }
                }
            }
        }
    }

    if settings.block_scripts_wad {
        blocked.push(SCRIPTS_WAD.to_string());
    }
    if !settings.patch_tft {
        blocked.push(TFT_WAD.to_string());
    }

    if cfg!(target_os = "macos") {
        for wad in MACOS_PLATFORM_WADS {
            blocked.push(wad.to_string());
        }
    }

    blocked.sort();
    blocked.dedup();
    blocked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_wad_header_matches_reference_algorithm() {
        use byteorder::{ReadBytesExt as _, LE};
        use ltk_wad::{WadBuilder, WadChunkBuilder};
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let wad_path = temp.path().join("Vayne.wad.client");
        let source_path = temp.path().join("Vayne.original.wad.client");
        let builder = WadBuilder::default()
            .with_chunk(WadChunkBuilder::default().with_hash(20))
            .with_chunk(WadChunkBuilder::default().with_hash(10));
        let mut output = File::create(&wad_path).unwrap();
        builder
            .build_to_writer(&mut output, |path_hash, cursor| {
                cursor.write_all(&path_hash.to_le_bytes())?;
                Ok(())
            })
            .unwrap();
        drop(output);

        std::fs::copy(&wad_path, &source_path).unwrap();
        let expected_signature = [0xA5_u8; WAD_V3_SIGNATURE_SIZE];
        let mut source = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&source_path)
            .unwrap();
        source
            .seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET))
            .unwrap();
        source.write_all(&expected_signature).unwrap();

        assert!(repair_macos_wad_header(&wad_path, &source_path).unwrap());
        assert!(!repair_macos_wad_header(&wad_path, &source_path).unwrap());

        let wad = ltk_wad::Wad::mount(File::open(&wad_path).unwrap()).unwrap();
        let mut checksum_input = vec![b'R', b'W', 3, 4];
        for chunk in wad.chunks() {
            checksum_input.extend_from_slice(&chunk.path_hash.to_le_bytes());
            checksum_input.extend_from_slice(&chunk.checksum.to_le_bytes());
        }
        let expected = xxhash_rust::xxh3::xxh3_64(&checksum_input);

        let mut file = File::open(&wad_path).unwrap();
        file.seek(SeekFrom::Start(WAD_V3_SIGNATURE_OFFSET)).unwrap();
        let mut actual_signature = [0_u8; WAD_V3_SIGNATURE_SIZE];
        file.read_exact(&mut actual_signature).unwrap();
        assert_eq!(actual_signature, expected_signature);
        file.seek(SeekFrom::Start(WAD_V3_CHECKSUM_OFFSET)).unwrap();
        assert_eq!(file.read_u64::<LE>().unwrap(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_wad_canonicalization_converts_multiframe_and_deduplicates() {
        use ltk_wad::{WadBuilder, WadChunkBuilder, WadChunkCompression};
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let wad_path = temp.path().join("Aatrox.wad.client");
        let source_path = temp.path().join("Aatrox.original.wad.client");
        let builder = WadBuilder::default()
            .with_chunk(WadChunkBuilder::default().with_hash(10))
            .with_chunk(WadChunkBuilder::default().with_hash(20));
        let mut output = File::create(&wad_path).unwrap();
        builder
            .build_to_writer(&mut output, |_path_hash, cursor| {
                cursor.write_all(b"shared chunk data")?;
                Ok(())
            })
            .unwrap();
        drop(output);
        std::fs::copy(&wad_path, &source_path).unwrap();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&wad_path)
            .unwrap();
        for index in 0..2 {
            file.seek(SeekFrom::Start(272 + index * 32 + 20)).unwrap();
            file.write_all(&[0x14]).unwrap();
        }
        drop(file);

        assert!(canonicalize_macos_wad(&wad_path, &source_path).unwrap());
        assert!(!canonicalize_macos_wad(&wad_path, &source_path).unwrap());

        let wad = ltk_wad::Wad::mount(File::open(&wad_path).unwrap()).unwrap();
        let chunks = wad.chunks().as_slice();
        assert_eq!(chunks.len(), 2);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.compression_type == WadChunkCompression::Zstd));
        assert_eq!(chunks[0].data_offset, chunks[1].data_offset);
        assert_eq!(chunks[0].checksum, chunks[1].checksum);
    }

    #[test]
    fn resolve_blocked_wads_exact_lowercased_and_scripts_added_by_default() {
        let settings = Settings {
            wad_blocklist: vec![WadBlocklistEntry::Exact {
                value: "Aatrox.wad.client".to_string(),
            }],
            ..Settings::default()
        };
        let result = resolve_blocked_wads(&settings, &[]);
        assert!(result.contains(&"aatrox.wad.client".to_string()));
        assert!(result.contains(&"scripts.wad.client".to_string()));
        assert!(result.contains(&"map22.wad.client".to_string()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolve_blocked_wads_includes_macos_platform_wads() {
        let settings = Settings {
            block_scripts_wad: false,
            patch_tft: true,
            ..Settings::default()
        };
        let result = resolve_blocked_wads(&settings, &[]);
        assert!(result.contains(&"bootstrap.macos.wad.client".to_string()));
        assert!(result.contains(&"shadercache.metal.wad.client".to_string()));
        assert!(result.contains(&"shaders.wad.client".to_string()));
    }

    #[test]
    fn resolve_blocked_wads_regex_expanded_against_available() {
        let settings = Settings {
            block_scripts_wad: false,
            patch_tft: true,
            wad_blocklist: vec![WadBlocklistEntry::Regex {
                value: r"^map\d+\.en_us\.wad\.client$".to_string(),
            }],
            ..Settings::default()
        };
        let available = vec![
            "map11.en_us.wad.client".to_string(),
            "map12.wad.client".to_string(),
            "map22.en_us.wad.client".to_string(),
            "aatrox.wad.client".to_string(),
        ];
        let result = resolve_blocked_wads(&settings, &available);
        assert!(result.contains(&"map11.en_us.wad.client".to_string()));
        assert!(result.contains(&"map22.en_us.wad.client".to_string()));
        assert!(!result.contains(&"map12.wad.client".to_string()));
        assert!(!result.contains(&"aatrox.wad.client".to_string()));
    }

    #[test]
    fn resolve_blocked_wads_invalid_regex_skipped_and_others_kept() {
        let settings = Settings {
            block_scripts_wad: false,
            patch_tft: true,
            wad_blocklist: vec![
                WadBlocklistEntry::Regex {
                    value: "[bad(".to_string(),
                },
                WadBlocklistEntry::Exact {
                    value: "keeper.wad.client".to_string(),
                },
            ],
            ..Settings::default()
        };
        let result = resolve_blocked_wads(&settings, &[]);
        assert!(result.contains(&"keeper.wad.client".to_string()));
    }

    #[test]
    fn resolve_blocked_wads_dedupes_overlapping_entries() {
        let settings = Settings {
            block_scripts_wad: true,
            patch_tft: true,
            wad_blocklist: vec![
                WadBlocklistEntry::Exact {
                    value: "Scripts.wad.client".to_string(),
                },
                WadBlocklistEntry::Regex {
                    value: "^scripts".to_string(),
                },
            ],
            ..Settings::default()
        };
        let available = vec!["scripts.wad.client".to_string()];
        let result = resolve_blocked_wads(&settings, &available);
        assert!(result.contains(&"scripts.wad.client".to_string()));
        let scripts_count = result.iter().filter(|w| *w == "scripts.wad.client").count();
        assert_eq!(scripts_count, 1);
    }

    #[test]
    fn overlay_stage_serialization() {
        assert_eq!(
            serde_json::to_string(&OverlayStage::Indexing).unwrap(),
            "\"indexing\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStage::Collecting).unwrap(),
            "\"collecting\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStage::Patching).unwrap(),
            "\"patching\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStage::Strings).unwrap(),
            "\"strings\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStage::Complete).unwrap(),
            "\"complete\""
        );
    }

    #[test]
    fn overlay_progress_serialization() {
        let progress = OverlayProgress {
            stage: OverlayStage::Patching,
            current_file: Some("test.wad.client".to_string()),
            current: 5,
            total: 10,
        };
        let json = serde_json::to_value(&progress).unwrap();
        assert_eq!(json["stage"], "patching");
        assert_eq!(json["currentFile"], "test.wad.client");
        assert_eq!(json["current"], 5);
        assert_eq!(json["total"], 10);
    }
}
