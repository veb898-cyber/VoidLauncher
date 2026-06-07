//! Shared utilities for fetching mod loader version lists from the
//! Prism Launcher metadata mirror.
//!
//! Prism maintains a curated, versioned JSON mirror at
//! `https://meta.prismlauncher.org/v1/<uid>/index.json` for every loader
//! it supports. This is *vastly* more reliable than hitting the upstream
//! APIs directly:
//!
//! * Single host — no DNS/network hop per loader.
//! * Curated subset — already filtered to the versions Prism supports.
//! * Uniform schema — every loader's `index.json` is shaped the same.
//! * Returns quickly (single-digit seconds even from cold cache, vs
//!   the 60s+ timeouts we see on `maven.neoforged.net`).
//!
//! Trade-off: we now depend on Prism's mirror uptime. If the mirror
//! ever moves, the `META_BASE` constant below is the only thing to
//! change. (Prism's CMakeLists.txt defines the same constant at
//! build-time, so this is the canonical public endpoint.)
//!
//! We use the mirror for *version listing only*. Install profiles
//! (the per-version `.json` that drives `get_profile` / `install`)
//! are still pulled from each loader's upstream source because the
//! Prism install JSONs contain Prism-specific wrappers (e.g. the
//! `zekerzhayard:ForgeWrapper` fork) that wouldn't work in our
//! launcher. The exception is LiteLoader, which has no working
//! upstream install profile and only lives on the Prism mirror.
//!
//! # Pagination
//!
//! The wizard uses infinite scroll: it asks for `PAGE_SIZE` items
//! at a time, then asks for the next page starting at
//! `accumulated.length`. To keep subsequent pages instant (no
//! re-download of the 1.96 MB Forge index on every scroll tick) we
//! cache the *raw, unfiltered, parsed* index per `uid` in a
//! process-wide `OnceLock<Mutex<HashMap<...>>>`. The first call
//! fetches from the network; every later call (including all
//! subsequent pages, even for a different MC version) just filters
//! the in-memory Vec and slices it. The filter is O(N) and the sort
//! is O(N log N), both trivially fast for N ≤ ~5000 (Forge is the
//! biggest).

use crate::error::Result;
use crate::modloaders::{LoaderVersion, LoaderVersionPage};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Public base of the Prism metadata mirror. Prism's own
/// `CMakeLists.txt:204` hard-codes the same URL.
const META_BASE: &str = "https://meta.prismlauncher.org/v1";

/// Per-request HTTP timeout. 60s is generous (Prism's biggest file
/// is Forge's index at ~2 MB and takes ~3-5s from a cold cache) but
/// matches the timeout we used pre-cache so behavior is consistent
/// for users who manage to evict the cache between page fetches.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Cached parsed index per `uid` (e.g. `net.minecraftforge`).
type CachedIndex = Arc<Vec<PrismVersionEntry>>;

static INDEX_CACHE: OnceLock<Mutex<HashMap<String, CachedIndex>>> = OnceLock::new();

fn index_cache() -> &'static Mutex<HashMap<String, CachedIndex>> {
    INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Top-level shape of `<uid>/index.json` — the same for every loader.
#[derive(Debug, Deserialize)]
struct PrismIndex {
    /// Prism's JSON key is `formatVersion` (camelCase). Without the
    /// rename, serde silently fails to deserialize the *whole* index
    /// (the field is required, no default) and our `match` in
    /// `fetch_loader_versions` swallows the error into an empty list
    /// — which is what made every loader look empty in the wizard
    /// until this rename was added. Don't drop the rename.
    #[serde(rename = "formatVersion", default)]
    #[allow(dead_code)]
    format_version: u32,
    /// If Prism ever drops the `name` field, fall back to `""` rather
    /// than failing the whole deserialize.
    #[serde(default)]
    #[allow(dead_code)]
    name: String,
    /// Same defensive default for `uid`.
    #[serde(default)]
    #[allow(dead_code)]
    uid: String,
    /// `versions` is the whole payload — if it's missing, just return
    /// an empty list (which is the same as a transient fetch failure).
    #[serde(default)]
    versions: Vec<PrismVersionEntry>,
}

/// A single entry inside `index.json`'s `versions` array.
#[derive(Debug, Deserialize)]
struct PrismVersionEntry {
    version: String,
    /// Prism's editor-chosen "this is the version we recommend for the
    /// matching MC version". We surface this as `stable: true`; all
    /// other entries default to `stable: false`.
    #[serde(default)]
    recommended: bool,
    /// `"release"` or `"snapshot"`. We don't filter on this — we let
    /// the user see beta builds — but we use it to derive `stable` for
    /// entries that aren't explicitly recommended.
    #[serde(default)]
    #[allow(dead_code)]
    r#type: String,
    /// What MC / mapping / dependency versions this loader build
    /// requires. For Forge/NeoForge/LiteLoader, one entry has
    /// `uid == "net.minecraft"` and `equals == "<mc version>"`. For
    /// Fabric/Quilt the only requirement is `net.fabricmc.intermediary`
    /// (no `equals`) so the loader works for *every* MC version.
    #[serde(default)]
    requires: Vec<PrismRequire>,
}

#[derive(Debug, Deserialize)]
struct PrismRequire {
    #[allow(dead_code)]
    uid: String,
    /// Missing for Fabric/Quilt (they don't pin a specific MC version
    /// here — the binding is in the intermediary mappings).
    #[serde(default)]
    equals: Option<String>,
}

/// Conservative list of substrings Prism uses to mark a build as a
/// pre-release. We use it only to derive the `stable` flag for
/// non-recommended entries — we still *display* these builds.
///
/// Case-insensitive on the `-snapshot` tag because LiteLoader
/// versions come back as `1.12.2-SNAPSHOT` (uppercase) — the
/// Prism convention is mixed and we don't want to miss any.
fn is_prerelease(version: &str) -> bool {
    let v = version.to_ascii_lowercase();
    v.contains("-beta")
        || v.contains("-alpha")
        || v.contains("-rc")
        || v.contains("-snapshot")
        || v.contains("-pre")
        || v.contains("-dev")
}

/// Get the cached parsed index for `uid`, fetching + parsing the
/// mirror JSON on first call. Subsequent calls return the cached
/// `Arc` clone (O(1)).
async fn get_or_fetch_index(uid: &str) -> Result<CachedIndex> {
    if let Some(cached) = index_cache().lock().unwrap().get(uid).cloned() {
        return Ok(cached);
    }
    let client = crate::download::global_http_client();
    let url = format!("{}/{}/index.json", META_BASE, uid);
    let resp_value = client
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(
                target: "launcher",
                "Failed to fetch Prism meta index (uid={}): {}",
                uid, e
            );
            e
        })?;
    let body = resp_value.text().await.map_err(|e| {
        tracing::error!(
            target: "launcher",
            "Failed to read Prism meta index body (uid={}): {}",
            uid, e
        );
        e
    })?;
    let index: PrismIndex = serde_json::from_str(&body).map_err(|e| {
        // Log the first 400 chars of the body so the next time
        // Prism changes its schema we can see *what* changed in
        // the launcher log file
        // (`%LOCALAPPDATA%\VoidLauncher\logs\launcher-*.log`),
        // not just "deserialize failed".
        let preview: String = body.chars().take(400).collect();
        tracing::error!(
            target: "launcher",
            "Failed to parse Prism meta index (uid={}): {} — body: {}",
            uid, e, preview
        );
        e
    })?;
    let entries = Arc::new(index.versions);
    index_cache()
        .lock()
        .unwrap()
        .insert(uid.to_string(), entries.clone());
    Ok(entries)
}

/// Filter `entries` by MC version, sort newest-first, and return
/// the page starting at `offset` of up to `limit` items plus the
/// total count. Extracted from `fetch_loader_versions` so the
/// pagination logic is testable without a network round-trip.
fn page_from(
    entries: &[PrismVersionEntry],
    mc_version: Option<&str>,
    offset: usize,
    limit: usize,
) -> (Vec<LoaderVersion>, usize) {
    let mut filtered: Vec<&PrismVersionEntry> = entries
        .iter()
        .filter(|entry| match mc_version {
            None => true,
            Some(mc) => entry.requires.iter().any(|r| {
                r.uid == "net.minecraft" && r.equals.as_deref() == Some(mc)
            }),
        })
        .collect();

    filtered.sort_by(|a, b| b.version.cmp(&a.version));

    let total = filtered.len();
    let page: Vec<LoaderVersion> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|entry| LoaderVersion {
            version: entry.version.clone(),
            // A version is "stable" if Prism marked it recommended, OR
            // if the version string contains none of the pre-release
            // tags we know about. Prism already filtered out garbage,
            // so this is just a defensive fallback.
            stable: entry.recommended || !is_prerelease(&entry.version),
        })
        .collect();
    (page, total)
}

/// Fetch one page of loader versions for `uid` from the Prism
/// mirror, optionally filtered to entries that target a specific
/// Minecraft version.
///
/// * `mc_version = None` → no MC filter (Fabric, Quilt).
/// * `mc_version = Some(mc)` → only entries whose `requires` array
///   contains `{uid: "net.minecraft", equals: mc}` (Forge,
///   NeoForge, LiteLoader).
///
/// `offset` and `limit` are into the *filtered, sorted (newest
/// first)* result. `offset >= total` returns an empty page with
/// the same `total` so the wizard can no-op without a second
/// request.
///
/// On *any* fetch/parse failure we log to the launcher log file
/// and return an empty page with `total = 0`. This is the same
/// fail-soft policy as the per-loader `get_loader_versions`
/// functions.
pub async fn fetch_loader_versions(
    uid: &str,
    mc_version: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    let entries = match get_or_fetch_index(uid).await {
        Ok(e) => e,
        Err(_) => return Ok(LoaderVersionPage { versions: Vec::new(), total: 0 }),
    };

    let (page, total) = page_from(&entries, mc_version, offset, limit);
    tracing::info!(
        target: "launcher",
        "Prism meta index paged: uid={}, mc_version={:?}, offset={}, limit={}, returned={}, total={}",
        uid, mc_version, offset, limit, page.len(), total
    );
    Ok(LoaderVersionPage { versions: page, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snippet of real Forge index data (verbatim from the Prism
    /// mirror) — this is what the function was failing on when the
    /// camelCase `formatVersion` key wasn't renamed. The test pins the
    /// full top-level + nested schema so future additions to the
    /// `PrismIndex` struct don't silently regress to "deserialize the
    /// whole 1.96MB file fails, every loader shows empty list".
    const SAMPLE_FORGE: &str = r#"{
        "formatVersion": 1,
        "name": "Forge",
        "uid": "net.minecraftforge",
        "versions": [
            {"version":"61.1.8","recommended":false,"releaseTime":"2026-05-27T16:11:23+00:00","requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"a"},
            {"version":"61.0.0","recommended":false,"releaseTime":"2026-05-01T00:00:00+00:00","requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"b"},
            {"version":"1.20.1-47.3.0","recommended":false,"releaseTime":"2024-01-01T00:00:00+00:00","requires":[{"uid":"net.minecraft","equals":"1.20.1"}],"sha256":"c"},
            {"version":"1.20.1-47.2.0","recommended":true,"releaseTime":"2023-12-01T00:00:00+00:00","requires":[{"uid":"net.minecraft","equals":"1.20.1"}],"sha256":"d"}
        ]
    }"#;

    #[test]
    fn parses_forge_index_with_formatversion_camelcase() {
        let parsed: PrismIndex = serde_json::from_str(SAMPLE_FORGE)
            .expect("SAMPLE_FORGE must parse — schema rename is the whole point of this test");
        assert_eq!(parsed.format_version, 1);
        assert_eq!(parsed.name, "Forge");
        assert_eq!(parsed.uid, "net.minecraftforge");
        assert_eq!(parsed.versions.len(), 4);
    }

    /// A real LiteLoader entry has a `type` field (unlike Forge/NeoForge
    /// which omit it). Make sure `type: "snapshot"` deserializes fine.
    const SAMPLE_LITELOADER: &str = r#"{
        "formatVersion": 1,
        "name": "LiteLoader",
        "uid": "com.mumfrey.liteloader",
        "versions": [
            {"version":"1.12.2-SNAPSHOT","recommended":false,"type":"snapshot","releaseTime":"2017-11-28T14:44:31+00:00","requires":[{"uid":"net.minecraft","equals":"1.12.2"}],"sha256":"x"}
        ]
    }"#;

    #[test]
    fn parses_liteloader_index_with_type_field() {
        let parsed: PrismIndex = serde_json::from_str(SAMPLE_LITELOADER)
            .expect("SAMPLE_LITELOADER must parse");
        assert_eq!(parsed.versions.len(), 1);
        assert_eq!(parsed.versions[0].version, "1.12.2-SNAPSHOT");
    }

    /// Fabric entry: no `type`, no `equals` in `requires` (universal
    /// across MC versions).
    const SAMPLE_FABRIC: &str = r#"{
        "formatVersion": 1,
        "name": "Fabric Loader",
        "uid": "net.fabricmc.fabric-loader",
        "versions": [
            {"version":"0.19.3","recommended":true,"type":"release","releaseTime":"2026-06-01T17:23:10+00:00","requires":[{"uid":"net.fabricmc.intermediary"}],"sha256":"x"}
        ]
    }"#;

    #[test]
    fn parses_fabric_index_without_equals() {
        let parsed: PrismIndex = serde_json::from_str(SAMPLE_FABRIC)
            .expect("SAMPLE_FABRIC must parse");
        assert_eq!(parsed.versions.len(), 1);
        assert!(parsed.versions[0].recommended);
        assert!(parsed.versions[0].requires[0].equals.is_none());
    }

    #[test]
    fn is_prerelease_detects_common_tags() {
        assert!(is_prerelease("1.0.0-beta.1"));
        assert!(is_prerelease("1.0.0-alpha"));
        assert!(is_prerelease("1.0.0-rc.1"));
        assert!(is_prerelease("1.12.2-SNAPSHOT"));
        assert!(is_prerelease("1.0.0-pre1"));
        assert!(is_prerelease("1.0.0-dev"));
        assert!(!is_prerelease("0.19.3"));
        assert!(!is_prerelease("61.1.8"));
        assert!(!is_prerelease("1.20.1-47.3.0"));
    }

    /// A 6-entry Forge index (verbatim schema) — the four 1.21.11
    /// entries will all match the MC filter; the two 1.20.1 entries
    /// will not. Verifies the filter, the sort (newest-first by
    /// version string), and the page slicing.
    const SAMPLE_PAGINATE: &str = r#"{
        "formatVersion": 1,
        "name": "Forge",
        "uid": "net.minecraftforge",
        "versions": [
            {"version":"61.1.8","recommended":false,"requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"a"},
            {"version":"61.0.0","recommended":false,"requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"b"},
            {"version":"60.1.0","recommended":false,"requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"c"},
            {"version":"60.0.5","recommended":true, "requires":[{"uid":"net.minecraft","equals":"1.21.11"}],"sha256":"d"},
            {"version":"1.20.1-47.3.0","recommended":false,"requires":[{"uid":"net.minecraft","equals":"1.20.1"}],"sha256":"e"},
            {"version":"1.20.1-47.2.0","recommended":false,"requires":[{"uid":"net.minecraft","equals":"1.20.1"}],"sha256":"f"}
        ]
    }"#;

    fn parsed_versions() -> Vec<PrismVersionEntry> {
        serde_json::from_str::<PrismIndex>(SAMPLE_PAGINATE)
            .expect("SAMPLE_PAGINATE must parse")
            .versions
    }

    #[test]
    fn page_filters_by_mc_version() {
        let entries = parsed_versions();
        let (page, total) = page_from(&entries, Some("1.21.11"), 0, 20);
        assert_eq!(total, 4, "4 entries target 1.21.11");
        assert_eq!(page.len(), 4);
        for v in &page {
            assert!(
                v.version.starts_with("60.") || v.version.starts_with("61."),
                "filtered out non-1.21.11 entry: {}",
                v.version
            );
        }
    }

    #[test]
    fn page_skips_mc_filter_when_none() {
        let entries = parsed_versions();
        let (page, total) = page_from(&entries, None, 0, 20);
        assert_eq!(total, 6, "no filter → all entries");
        assert_eq!(page.len(), 6);
    }

    #[test]
    fn page_sorts_newest_first_by_version_string() {
        let entries = parsed_versions();
        let (page, _) = page_from(&entries, Some("1.21.11"), 0, 20);
        // String sort descending: 61.1.8 > 61.0.0 > 60.1.0 > 60.0.5
        assert_eq!(page[0].version, "61.1.8");
        assert_eq!(page[1].version, "61.0.0");
        assert_eq!(page[2].version, "60.1.0");
        assert_eq!(page[3].version, "60.0.5");
    }

    #[test]
    fn page_returns_correct_slice_for_offset() {
        let entries = parsed_versions();
        // First page
        let (p1, total1) = page_from(&entries, Some("1.21.11"), 0, 2);
        assert_eq!(total1, 4);
        assert_eq!(p1.len(), 2);
        assert_eq!(p1[0].version, "61.1.8");
        assert_eq!(p1[1].version, "61.0.0");
        // Second page
        let (p2, total2) = page_from(&entries, Some("1.21.11"), 2, 2);
        assert_eq!(total2, 4);
        assert_eq!(p2.len(), 2);
        assert_eq!(p2[0].version, "60.1.0");
        assert_eq!(p2[1].version, "60.0.5");
    }

    #[test]
    fn page_returns_empty_when_offset_past_total() {
        let entries = parsed_versions();
        let (page, total) = page_from(&entries, Some("1.21.11"), 10, 20);
        assert_eq!(total, 4);
        assert!(page.is_empty(), "offset >= total → empty page, same total");
    }

    #[test]
    fn page_marks_recommended_as_stable() {
        let entries = parsed_versions();
        let (page, _) = page_from(&entries, Some("1.21.11"), 0, 20);
        let by_v: std::collections::HashMap<&str, bool> =
            page.iter().map(|v| (v.version.as_str(), v.stable)).collect();
        assert!(by_v["60.0.5"], "recommended → stable");
        // 61.x and 60.1.0 are not prerelease by our heuristic
        // (no -beta/-alpha/-rc/-snapshot/-pre/-dev substring) and
        // are not recommended, so they fall through to `!is_prerelease`
        // → stable = true. That's fine for Forge where the version
        // string is the source of truth for pre-release status.
        assert!(by_v["61.1.8"]);
    }
}
