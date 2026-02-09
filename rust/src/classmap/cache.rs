use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

pub(crate) const CACHE_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct CachedFile {
    pub mtime: u64,
    pub symbols: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct CacheData {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub files: HashMap<String, CachedFile>,
    #[serde(default)]
    pub dir_mtimes: HashMap<String, u64>,
}

pub(crate) fn load_cache(cache_path: &Path) -> CacheData {
    fs::read(cache_path)
        .ok()
        .and_then(|data| serde_json::from_slice::<CacheData>(&data).ok())
        .filter(|c| c.version == CACHE_VERSION)
        .unwrap_or_default()
}

pub(crate) fn save_cache(cache_path: &Path, cache: &CacheData) {
    if let Ok(data) = serde_json::to_vec(cache) {
        let _ = fs::write(cache_path, data);
    }
}

pub(crate) fn get_mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Check if all cached directory mtimes match the filesystem.
/// Returns true if the walk can be skipped entirely.
pub(crate) fn dirs_unchanged(cache: &CacheData, dirs: &[&str]) -> bool {
    if cache.dir_mtimes.is_empty() || cache.files.is_empty() {
        return false;
    }

    // Every root dir must be represented in the cache
    for dir in dirs {
        let p = Path::new(dir);
        if p.is_file() {
            continue;
        }
        if p.exists() && !cache.dir_mtimes.contains_key(*dir) {
            return false;
        }
    }

    // All cached directories must have unchanged mtimes
    for (dir, &cached_mtime) in &cache.dir_mtimes {
        let current_mtime = get_mtime(Path::new(dir));
        if current_mtime != cached_mtime || current_mtime == 0 {
            return false;
        }
    }

    true
}
