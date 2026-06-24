//! Pre-patch validation of property-bin "linked file" dependencies.
//!
//! League property-bins (`PROP`/`PTCH`) declare a list of *linked* bin paths they
//! depend on. At load time the game resolves each linked path against the mounted
//! WADs; a missing dependency yields `STATUS_NOT_FOUND` (`c0000225`). The cslol
//! patcher used to treat this as fatal but now only logs it and keeps patching, so
//! a broken mod can silently destabilize the game.
//!
//! We replicate the check here and run it *before* injection: a linked bin is
//! considered missing when its chunk path hash is absent from the union of the base
//! game chunks and every enabled mod's chunks. Overlays only add or replace chunks
//! (never remove base ones), so that union is exactly the view the game mounts —
//! making this an accurate, proactive replica of the runtime check.

use std::collections::{HashMap, HashSet};

use camino::{Utf8Path, Utf8PathBuf};
use ltk_overlay::utils::resolve_chunk_hash;
use ltk_overlay::GameIndex;

use crate::error::{AppError, AppResult, Utf8PathExt};
use crate::mods::ModLibrary;
use crate::state::Settings;

/// Upper bound on a bin's declared linked-file count, guarding `Vec` pre-allocation
/// against corrupt/garbage input. Real bins declare at most a handful.
const MAX_LINKED_FILES: u32 = 100_000;

/// A library mod that ships one or more property-bins whose linked dependencies
/// cannot be resolved against the current game + enabled-mod set.
#[derive(Debug, Clone)]
pub struct LinkedBinOffender {
    /// Library mod id (matches `InstalledMod.id` on the frontend).
    pub mod_id: String,
    /// Mod display name, as a fallback when the frontend can't resolve the id.
    pub display_name: String,
    /// WAD targets (e.g. `Ahri.wad.client`) in this mod that contain the unresolved
    /// bins. May be empty when the offending bin came from a RAW override.
    pub wads: Vec<String>,
    /// The missing linked bin paths, deduped and sorted.
    pub missing_links: Vec<String>,
}

/// One bin's unresolved linked dependency, recorded during the collection pass and
/// resolved against the full present-set afterwards.
struct LinkRecord {
    mod_index: usize,
    /// WAD target the bin lives in, or `None` for RAW overrides.
    wad: Option<String>,
    linked_path: String,
    link_hash: u64,
}

impl ModLibrary {
    /// Scan every enabled library mod for property-bins whose linked dependencies
    /// cannot be resolved against the base game plus the enabled-mod set.
    ///
    /// Returns one [`LinkedBinOffender`] per mod with unresolved links (empty when
    /// everything resolves). Workshop projects are out of scope — this validates the
    /// active profile's enabled library mods only. WADs on the user's blocklist are
    /// excluded so the pre-flight matches the overlay the patcher actually builds.
    pub fn validate_linked_bins(&self, settings: &Settings) -> AppResult<Vec<LinkedBinOffender>> {
        let storage_dir = self.storage_dir(settings)?;
        let game_dir = crate::utils::game::resolve_game_dir(settings)?;
        let (profile_slug, mut enabled_mods) = self.get_enabled_mods_for_overlay(settings)?;

        if enabled_mods.is_empty() {
            return Ok(Vec::new());
        }

        // Mirror `ensure_overlay`: blocklisted WADs are never patched into the
        // overlay, so their bins must be excluded here too. Including them would
        // flag deps for bins the game never loads (false positives) and count their
        // chunks as present (masking genuinely missing deps).
        let available_wads = crate::utils::game::list_game_wads(&game_dir).unwrap_or_default();
        let blocked_wads: HashSet<String> = super::resolve_blocked_wads(settings, &available_wads)
            .into_iter()
            .collect();

        let profile_dir = storage_dir.join("profiles").join(profile_slug.as_str());
        let utf8_game_dir = game_dir.try_into_utf8("game directory")?;
        let utf8_cache_path = profile_dir
            .join("game_index.bin")
            .try_into_utf8("game index cache path")?;

        // Reuses the same cache the overlay build writes, so this is warm in the
        // common case (no game patch since the last build).
        let game_index = GameIndex::load_or_build(&utf8_game_dir, &utf8_cache_path)
            .map_err(|e| AppError::Other(format!("Failed to build game index: {}", e)))?;

        let mut present: HashSet<u64> = game_index.hash_index.keys().copied().collect();
        let mut display_names: Vec<String> = Vec::with_capacity(enabled_mods.len());
        let mut records: Vec<LinkRecord> = Vec::new();

        for (mod_index, em) in enabled_mods.iter_mut().enumerate() {
            let project = em
                .content
                .mod_project()
                .map_err(|e| AppError::Other(format!("Failed to read mod project: {}", e)))?;
            display_names.push(project.display_name.clone());

            // (wad target | None for RAW, in-wad path, bytes)
            let mut overrides: Vec<(Option<String>, Utf8PathBuf, Vec<u8>)> = Vec::new();
            for layer in &project.layers {
                if !em.is_layer_active(&layer.name) {
                    continue;
                }
                let wads = em
                    .content
                    .list_layer_wads(&layer.name)
                    .map_err(|e| AppError::Other(format!("Failed to list layer wads: {}", e)))?;
                for wad in &wads {
                    if blocked_wads.contains(&wad.to_lowercase()) {
                        continue;
                    }
                    let wad_overrides =
                        em.content
                            .read_wad_overrides(&layer.name, wad)
                            .map_err(|e| {
                                AppError::Other(format!("Failed to read wad overrides: {}", e))
                            })?;
                    for (rel_path, bytes) in wad_overrides {
                        overrides.push((Some(wad.clone()), rel_path, bytes));
                    }
                }
            }
            let raw_overrides = em
                .content
                .read_raw_overrides()
                .map_err(|e| AppError::Other(format!("Failed to read raw overrides: {}", e)))?;
            for (rel_path, bytes) in raw_overrides {
                overrides.push((None, rel_path, bytes));
            }

            for (wad, rel_path, bytes) in &overrides {
                // Every override contributes a chunk the game will see.
                if let Ok(hash) = resolve_chunk_hash(rel_path, bytes) {
                    present.insert(hash);
                }
                // Property bins may declare linked dependencies to resolve later.
                if let Some(links) = parse_linked_bins(bytes) {
                    for linked in links {
                        if let Ok(link_hash) = resolve_chunk_hash(Utf8Path::new(&linked), b"") {
                            records.push(LinkRecord {
                                mod_index,
                                wad: wad.clone(),
                                linked_path: linked,
                                link_hash,
                            });
                        }
                    }
                }
            }
        }

        let mod_ids: Vec<String> = enabled_mods.iter().map(|m| m.id.clone()).collect();
        let offenders = collect_offenders(&present, &records, &mod_ids, &display_names);
        tracing::debug!(
            enabled_mods = enabled_mods.len(),
            ?blocked_wads,
            present_chunks = present.len(),
            linked_records = records.len(),
            offenders = offenders.len(),
            "Linked-bin pre-flight complete",
        );
        Ok(offenders)
    }
}

/// Group unresolved [`LinkRecord`]s into per-mod offenders. Pure (no I/O) so the
/// resolution logic can be unit-tested independently of mod/game fixtures.
fn collect_offenders(
    present: &HashSet<u64>,
    records: &[LinkRecord],
    mod_ids: &[String],
    display_names: &[String],
) -> Vec<LinkedBinOffender> {
    let mut by_mod: HashMap<usize, (HashSet<String>, HashSet<String>)> = HashMap::new();
    for rec in records {
        if present.contains(&rec.link_hash) {
            continue;
        }
        let entry = by_mod.entry(rec.mod_index).or_default();
        if let Some(wad) = &rec.wad {
            entry.0.insert(wad.clone());
        }
        entry.1.insert(rec.linked_path.clone());
    }

    let mut offenders: Vec<LinkedBinOffender> = by_mod
        .into_iter()
        .map(|(mod_index, (wads, links))| {
            let mut wads: Vec<String> = wads.into_iter().collect();
            wads.sort();
            let mut missing_links: Vec<String> = links.into_iter().collect();
            missing_links.sort();
            LinkedBinOffender {
                mod_id: mod_ids[mod_index].clone(),
                display_name: display_names[mod_index].clone(),
                wads,
                missing_links,
            }
        })
        .collect();
    offenders.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    offenders
}

/// Parse the "linked files" list from a League property-bin.
///
/// Layout (little-endian):
/// - optional `PTCH` magic (4) + patch header `(u32, u32)`
/// - `PROP` magic (4) + `version: u32`
/// - if `version >= 2`: `count: u32`, then `count` × (`len: u16` + `len` UTF-8 bytes)
///
/// Returns `Some(links)` for a well-formed bin (empty when it declares none) and
/// `None` when the bytes are not a property-bin or are truncated.
fn parse_linked_bins(bytes: &[u8]) -> Option<Vec<String>> {
    use byteorder::{ReadBytesExt, LE};
    use std::io::Read;

    let mut cursor = std::io::Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic).ok()?;

    if &magic == b"PTCH" {
        // Patch header: two u32s precede the embedded PROP section.
        cursor.read_u32::<LE>().ok()?;
        cursor.read_u32::<LE>().ok()?;
        cursor.read_exact(&mut magic).ok()?;
    }

    if &magic != b"PROP" {
        return None;
    }

    let version = cursor.read_u32::<LE>().ok()?;
    if version < 2 {
        return Some(Vec::new());
    }

    let count = cursor.read_u32::<LE>().ok()?;
    if count > MAX_LINKED_FILES {
        return None;
    }

    let mut links = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = cursor.read_u16::<LE>().ok()? as usize;
        let mut buf = vec![0u8; len];
        cursor.read_exact(&mut buf).ok()?;
        links.push(String::from_utf8_lossy(&buf).into_owned());
    }
    Some(links)
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::{WriteBytesExt, LE};
    use std::io::Write;

    /// Build a minimal PROP bin body with the given version and linked paths.
    fn prop_bin(version: u32, linked: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"PROP");
        buf.write_u32::<LE>(version).unwrap();
        if version >= 2 {
            buf.write_u32::<LE>(linked.len() as u32).unwrap();
            for path in linked {
                buf.write_u16::<LE>(path.len() as u16).unwrap();
                buf.write_all(path.as_bytes()).unwrap();
            }
        }
        // Trailing object-type count (unused by the parser) to mimic a real file.
        buf.write_u32::<LE>(0).unwrap();
        buf
    }

    /// Wrap a PROP body in a PTCH patch header.
    fn ptch_bin(version: u32, linked: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"PTCH");
        buf.write_u32::<LE>(1).unwrap();
        buf.write_u32::<LE>(0).unwrap();
        buf.extend_from_slice(&prop_bin(version, linked));
        buf
    }

    #[test]
    fn parses_v1_bin_as_no_links() {
        assert_eq!(parse_linked_bins(&prop_bin(1, &[])), Some(Vec::new()));
    }

    #[test]
    fn parses_v2_linked_files() {
        let bin = prop_bin(
            3,
            &[
                "data/characters/ahri/ahri.bin",
                "data/characters/ahri/skins/skin0.bin",
            ],
        );
        assert_eq!(
            parse_linked_bins(&bin),
            Some(vec![
                "data/characters/ahri/ahri.bin".to_string(),
                "data/characters/ahri/skins/skin0.bin".to_string(),
            ])
        );
    }

    #[test]
    fn parses_ptch_wrapped_prop() {
        let bin = ptch_bin(3, &["data/characters/ahri/ahri.bin"]);
        assert_eq!(
            parse_linked_bins(&bin),
            Some(vec!["data/characters/ahri/ahri.bin".to_string()])
        );
    }

    #[test]
    fn rejects_non_bin_bytes() {
        assert_eq!(parse_linked_bins(b"OEGM\x01\x02\x03\x04"), None);
        assert_eq!(parse_linked_bins(&[]), None);
    }

    #[test]
    fn rejects_truncated_link_section() {
        // PROP v2 claiming one link but providing no string bytes.
        let mut bin = Vec::new();
        bin.extend_from_slice(b"PROP");
        bin.write_u32::<LE>(2).unwrap();
        bin.write_u32::<LE>(1).unwrap();
        bin.write_u16::<LE>(10).unwrap(); // declares 10 bytes that aren't there
        assert_eq!(parse_linked_bins(&bin), None);
    }

    #[test]
    fn rejects_absurd_link_count() {
        let mut bin = Vec::new();
        bin.extend_from_slice(b"PROP");
        bin.write_u32::<LE>(2).unwrap();
        bin.write_u32::<LE>(u32::MAX).unwrap();
        assert_eq!(parse_linked_bins(&bin), None);
    }

    fn hash(path: &str) -> u64 {
        resolve_chunk_hash(Utf8Path::new(path), b"").unwrap()
    }

    #[test]
    fn flags_only_unresolved_links_grouped_by_mod() {
        let present: HashSet<u64> = [hash("data/present/base.bin")].into_iter().collect();
        let mod_ids = vec!["mod-a".to_string(), "mod-b".to_string()];
        let display_names = vec!["Mod A".to_string(), "Mod B".to_string()];
        let records = vec![
            // resolved -> ignored
            LinkRecord {
                mod_index: 0,
                wad: Some("Ahri.wad.client".to_string()),
                linked_path: "data/present/base.bin".to_string(),
                link_hash: hash("data/present/base.bin"),
            },
            // missing, mod A, two wads, deduped path
            LinkRecord {
                mod_index: 0,
                wad: Some("Ahri.wad.client".to_string()),
                linked_path: "data/missing/x.bin".to_string(),
                link_hash: hash("data/missing/x.bin"),
            },
            LinkRecord {
                mod_index: 0,
                wad: Some("Khazix.wad.client".to_string()),
                linked_path: "data/missing/x.bin".to_string(),
                link_hash: hash("data/missing/x.bin"),
            },
            // missing, mod B, raw override (no wad)
            LinkRecord {
                mod_index: 1,
                wad: None,
                linked_path: "data/missing/y.bin".to_string(),
                link_hash: hash("data/missing/y.bin"),
            },
        ];

        let offenders = collect_offenders(&present, &records, &mod_ids, &display_names);
        assert_eq!(offenders.len(), 2);

        let a = &offenders[0];
        assert_eq!(a.mod_id, "mod-a");
        assert_eq!(a.wads, vec!["Ahri.wad.client", "Khazix.wad.client"]);
        assert_eq!(a.missing_links, vec!["data/missing/x.bin"]);

        let b = &offenders[1];
        assert_eq!(b.mod_id, "mod-b");
        assert!(b.wads.is_empty());
        assert_eq!(b.missing_links, vec!["data/missing/y.bin"]);
    }

    #[test]
    fn clean_set_yields_no_offenders() {
        let present: HashSet<u64> = [hash("data/present/base.bin")].into_iter().collect();
        let records = vec![LinkRecord {
            mod_index: 0,
            wad: Some("Ahri.wad.client".to_string()),
            linked_path: "data/present/base.bin".to_string(),
            link_hash: hash("data/present/base.bin"),
        }];
        let offenders = collect_offenders(
            &present,
            &records,
            &["mod-a".to_string()],
            &["Mod A".to_string()],
        );
        assert!(offenders.is_empty());
    }
}
