use ignore::WalkBuilder;
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::cache::{get_mtime, dirs_unchanged, CacheData, CachedFile, CACHE_VERSION};
use super::parser::{contains_class_keyword, extract_php_symbols};

pub(crate) type ParseResult = Option<(Vec<(String, String)>, String, CachedFile)>;

pub(crate) struct WalkResult {
    pub entries: Vec<(String, String)>,
    pub files_scanned: usize,
    pub php_files_found: usize,
    pub directories_walked: usize,
    pub cache_hits: usize,
    pub new_cache: CacheData,
    pub walk_skipped: bool,
}

enum WalkEntry {
    File(PathBuf),
    Dir(PathBuf, u64), // path, mtime
}

pub(crate) fn walk_and_parse(
    dirs: &[&str],
    excludes: &[Regex],
    cache: &CacheData,
    vendor_dir: &str,
) -> WalkResult {
    // Fast path: if all directory mtimes match cache, skip the walk entirely
    // and use cached file paths directly. This avoids readdir + stat on
    // thousands of non-PHP files in vendor/.
    if dirs_unchanged(cache, dirs) {
        return walk_and_parse_cached(dirs, excludes, cache, vendor_dir);
    }

    walk_and_parse_full(dirs, excludes, cache)
}

/// Fast path: skip directory walk, trust cache for vendor files.
///
/// Vendor files (under vendor_dir) never get edited in-place — packages are
/// installed/removed as a whole, which changes directory mtimes. Since
/// dirs_unchanged() already confirmed all dir mtimes match, we can trust
/// the cache for vendor files without any stat calls.
///
/// Non-vendor files (app source) may be edited in-place without changing
/// dir mtime, so we still do per-file mtime checks for those.
fn walk_and_parse_cached(
    dirs: &[&str],
    excludes: &[Regex],
    cache: &CacheData,
    vendor_dir: &str,
) -> WalkResult {
    // Partition cached files into vendor (trust cache) and non-vendor (need stat)
    let mut vendor_entries: Vec<(String, String)> = Vec::new();
    let mut vendor_files: HashMap<String, CachedFile> = HashMap::new();
    let mut non_vendor_paths: Vec<PathBuf> = Vec::new();
    let mut php_files_found: usize = 0;
    let mut vendor_files_with_symbols: usize = 0;

    for (path_str, cached) in &cache.files {
        let belongs = dirs.iter().any(|d| {
            let d_path = Path::new(d);
            if d_path.is_file() {
                Path::new(path_str.as_str()) == d_path
            } else {
                path_str.starts_with(d)
            }
        });
        if !belongs {
            continue;
        }
        if excludes.iter().any(|re| re.is_match(path_str)) {
            continue;
        }

        php_files_found += 1;

        if path_str.starts_with(vendor_dir) {
            // Vendor file: trust cache entirely — no stat needed
            if !cached.symbols.is_empty() {
                vendor_files_with_symbols += 1;
                for symbol in &cached.symbols {
                    vendor_entries.push((symbol.clone(), path_str.clone()));
                }
            }
            vendor_files.insert(path_str.clone(), cached.clone());
        } else {
            // Non-vendor file: needs stat to detect in-place edits
            non_vendor_paths.push(PathBuf::from(path_str));
        }
    }

    // Also add individual file paths from dirs
    for dir in dirs {
        let p = Path::new(dir);
        if p.is_file()
            && p.extension().is_some_and(|e| e == "php")
            && !vendor_files.contains_key(&p.to_string_lossy().into_owned())
            && !non_vendor_paths.iter().any(|existing| existing == p)
        {
            non_vendor_paths.push(p.to_path_buf());
            php_files_found += 1;
        }
    }

    // Stat + parse non-vendor files in parallel (typically a small set)
    let files_scanned = AtomicUsize::new(0);
    let cache_hit_count = AtomicUsize::new(0);

    let non_vendor_results: Vec<ParseResult> = non_vendor_paths
        .par_iter()
        .map(|path| parse_one_file(path, &cache.files, &files_scanned, &cache_hit_count))
        .collect();

    let mut all_entries = vendor_entries;
    let mut new_files = vendor_files;

    for result in non_vendor_results.into_iter().flatten() {
        let (file_entries, path_str, cache_entry) = result;
        all_entries.extend(file_entries);
        new_files.insert(path_str, cache_entry);
    }

    let total_cache_hits =
        cache_hit_count.load(Ordering::Relaxed) + php_files_found - non_vendor_paths.len();

    WalkResult {
        entries: all_entries,
        files_scanned: files_scanned.load(Ordering::Relaxed) + vendor_files_with_symbols,
        php_files_found,
        directories_walked: 0,
        cache_hits: total_cache_hits,
        new_cache: CacheData {
            version: CACHE_VERSION,
            files: new_files,
            dir_mtimes: cache.dir_mtimes.clone(),
        },
        walk_skipped: true,
    }
}

/// Full path: walk all directories, parse PHP files, collect dir mtimes.
fn walk_and_parse_full(
    dirs: &[&str],
    excludes: &[Regex],
    cache: &CacheData,
) -> WalkResult {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut walk_dirs: Vec<&str> = Vec::new();

    for dir in dirs {
        let p = Path::new(dir);
        if !p.exists() {
            continue;
        }
        if p.is_file() {
            if p.extension().is_some_and(|e| e == "php") {
                paths.push(p.to_path_buf());
            }
        } else {
            walk_dirs.push(dir);
        }
    }

    let dir_count = walk_dirs.len();
    let mut dir_mtimes: HashMap<String, u64> = HashMap::new();

    if !walk_dirs.is_empty() {
        let mut builder = WalkBuilder::new(walk_dirs[0]);
        builder
            .hidden(false)
            .git_ignore(false)
            .threads(num_cpus());

        for dir in &walk_dirs[1..] {
            builder.add(dir);
        }

        let (tx, rx) = std::sync::mpsc::channel::<WalkEntry>();

        let excludes_clone: Vec<Regex> = excludes.to_vec();
        builder.build_parallel().run(|| {
            let tx = tx.clone();
            let excludes = excludes_clone.clone();
            Box::new(move |entry| {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return ignore::WalkState::Continue,
                };

                let path = entry.path();
                let ft = match entry.file_type() {
                    Some(ft) => ft,
                    None => return ignore::WalkState::Continue,
                };

                if ft.is_dir() {
                    let mtime = get_mtime(path);
                    let _ = tx.send(WalkEntry::Dir(path.to_path_buf(), mtime));
                    return ignore::WalkState::Continue;
                }

                if !ft.is_file() {
                    return ignore::WalkState::Continue;
                }

                if path.extension().is_none_or(|e| e != "php") {
                    return ignore::WalkState::Continue;
                }

                if excludes
                    .iter()
                    .any(|re: &Regex| re.is_match(&path.to_string_lossy()))
                {
                    return ignore::WalkState::Continue;
                }

                let _ = tx.send(WalkEntry::File(path.to_path_buf()));
                ignore::WalkState::Continue
            })
        });

        drop(tx);
        for entry in rx {
            match entry {
                WalkEntry::File(p) => paths.push(p),
                WalkEntry::Dir(p, mtime) => {
                    dir_mtimes.insert(p.to_string_lossy().into_owned(), mtime);
                }
            }
        }
    }

    let php_files_found = paths.len();
    let files_scanned = AtomicUsize::new(0);
    let cache_hit_count = AtomicUsize::new(0);

    let results: Vec<ParseResult> = paths
        .par_iter()
        .map(|path| parse_one_file(path, &cache.files, &files_scanned, &cache_hit_count))
        .collect();

    let mut entries: Vec<(String, String)> = Vec::new();
    let mut new_files: HashMap<String, CachedFile> = HashMap::with_capacity(results.len());
    for result in results.into_iter().flatten() {
        let (file_entries, path_str, cache_entry) = result;
        entries.extend(file_entries);
        new_files.insert(path_str, cache_entry);
    }

    WalkResult {
        entries,
        files_scanned: files_scanned.load(Ordering::Relaxed),
        php_files_found,
        directories_walked: dir_count,
        cache_hits: cache_hit_count.load(Ordering::Relaxed),
        new_cache: CacheData {
            version: CACHE_VERSION,
            files: new_files,
            dir_mtimes,
        },
        walk_skipped: false,
    }
}

/// Parse a single PHP file, using cache if mtime matches.
fn parse_one_file(
    path: &Path,
    file_cache: &HashMap<String, CachedFile>,
    files_scanned: &AtomicUsize,
    cache_hit_count: &AtomicUsize,
) -> ParseResult {
    let path_str = path.to_string_lossy().into_owned();
    let mtime = get_mtime(path);

    if let Some(cached) = file_cache.get(&path_str) {
        if cached.mtime == mtime {
            cache_hit_count.fetch_add(1, Ordering::Relaxed);
            if !cached.symbols.is_empty() {
                files_scanned.fetch_add(1, Ordering::Relaxed);
            }
            let entries: Vec<(String, String)> = cached
                .symbols
                .iter()
                .map(|s| (s.clone(), path_str.clone()))
                .collect();
            return Some((entries, path_str, cached.clone()));
        }
    }

    let contents = match fs::read(path) {
        Ok(c) => c,
        Err(_) => return None,
    };

    if !contains_class_keyword(&contents) {
        return Some((
            vec![],
            path_str,
            CachedFile {
                mtime,
                symbols: vec![],
            },
        ));
    }

    let text = String::from_utf8_lossy(&contents);
    let symbols = extract_php_symbols(&text);
    let cache_entry = CachedFile {
        mtime,
        symbols: symbols.clone(),
    };

    if !symbols.is_empty() {
        files_scanned.fetch_add(1, Ordering::Relaxed);
    }

    let entries: Vec<(String, String)> = symbols
        .into_iter()
        .map(|s| (s, path_str.clone()))
        .collect();
    Some((entries, path_str, cache_entry))
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().max(2) - 1)
        .unwrap_or(4)
}
