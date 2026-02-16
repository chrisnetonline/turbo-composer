mod cache;
mod codegen;
mod parser;
mod walker;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use cache::{load_cache, save_cache, CacheData};
use codegen::{
    generate_autoload_php, generate_autoload_real_php, generate_classmap_file,
    generate_files_file, generate_namespaces_file, generate_psr4_file, generate_static_file,
};
use walker::walk_and_parse;

#[derive(Debug, Deserialize, Default, Clone)]
pub struct AutoloadMappings {
    #[serde(default, rename = "psr-4")]
    pub psr4: Vec<NamespaceMapping>,
    #[serde(default, rename = "psr-0")]
    pub psr0: Vec<NamespaceMapping>,
    #[serde(default)]
    pub classmap: Vec<String>,
    #[serde(default)]
    pub files: Vec<FileAutoload>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NamespaceMapping {
    pub namespace: String,
    pub path: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileAutoload {
    pub identifier: String,
    pub path: String,
}

pub struct ClassmapConfig {
    pub project_dir: String,
    pub vendor_dir: String,
    pub autoload: AutoloadMappings,
    pub exclude_from_classmap: Vec<String>,
    pub target_dir: Option<String>,
    pub suffix: Option<String>,
    pub write_files: bool,
    pub staging_suffix: Option<String>,
    pub has_platform_check: bool,
    pub has_files_autoload: bool,
}

#[derive(Debug, Serialize)]
struct Output {
    classmap_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    classmap_file_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    static_file_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    psr4_file_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespaces_file_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files_file_content: Option<String>,
    files_written: bool,
    stats: Stats,
}

#[derive(Debug, Serialize)]
struct Stats {
    files_scanned: usize,
    php_files_found: usize,
    directories_walked: usize,
    cache_hits: usize,
    walk_skipped: bool,
    elapsed_ms: u128,
    walk_ms: u128,
    parse_ms: u128,
    generate_ms: u128,
}

pub fn run(config: ClassmapConfig) -> serde_json::Value {
    let start = std::time::Instant::now();

    let excludes: Vec<Regex> = config
        .exclude_from_classmap
        .iter()
        .filter_map(|p| {
            // Patterns arrive as pre-built regex from PHP (absolute-path-anchored),
            // matching Composer's exclude-from-classmap resolution.
            Regex::new(p).ok()
        })
        .collect();

    // Skip fs::canonicalize syscall for absolute paths without ".." components
    let all_dirs: Vec<String> = config
        .autoload
        .psr4
        .iter()
        .map(|m| m.path.as_str())
        .chain(config.autoload.psr0.iter().map(|m| m.path.as_str()))
        .chain(config.autoload.classmap.iter().map(String::as_str))
        .map(|d| {
            if Path::new(d).is_absolute() && !d.contains("..") {
                d.to_string()
            } else {
                fs::canonicalize(d)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| d.to_string())
            }
        })
        .collect();

    let dir_refs: Vec<&str> = all_dirs.iter().map(String::as_str).collect();

    let cache_path = config
        .target_dir
        .as_ref()
        .map(|td| Path::new(td).join(".turbo-cache"));
    let cache: CacheData = cache_path
        .as_ref()
        .map(|p| load_cache(p))
        .unwrap_or_default();

    let vendor_real =
        fs::canonicalize(&config.vendor_dir).unwrap_or_else(|_| PathBuf::from(&config.vendor_dir));
    let vendor_str = vendor_real.to_string_lossy().to_string();

    let walk_parse_start = std::time::Instant::now();
    let walk_result = walk_and_parse(&dir_refs, &excludes, &cache, &vendor_str);
    let walk_parse_ms = walk_parse_start.elapsed().as_millis();

    let sort_start = std::time::Instant::now();

    // Resolve PSR-4/PSR-0/classmap base paths using the same logic as `all_dirs`
    // so that prefix-matching against walker output is consistent.
    let resolve_path = |d: &str| -> String {
        if Path::new(d).is_absolute() && !d.contains("..") {
            d.to_string()
        } else {
            fs::canonicalize(d)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| d.to_string())
        }
    };

    let psr4_resolved: Vec<(String, String)> = config
        .autoload
        .psr4
        .iter()
        .map(|m| (m.namespace.clone(), resolve_path(&m.path)))
        .collect();

    let psr0_resolved: Vec<(String, String)> = config
        .autoload
        .psr0
        .iter()
        .map(|m| (m.namespace.clone(), resolve_path(&m.path)))
        .collect();

    let classmap_resolved: Vec<String> = config
        .autoload
        .classmap
        .iter()
        .map(|d| resolve_path(d))
        .collect();

    // Use first-wins semantics to match Composer's behaviour.
    // Filter classes by PSR-4/PSR-0 compliance — Composer only includes
    // classes whose FQCN maps to the correct filename under the namespace
    // mapping. Secondary classes in a file (that don't match the filename)
    // are excluded, matching Composer's `filterByNamespace()` logic.
    let mut classmap: BTreeMap<String, String> = BTreeMap::new();
    for (class, path) in &walk_result.entries {
        if is_class_valid(
            class,
            path,
            &psr4_resolved,
            &psr0_resolved,
            &classmap_resolved,
        ) {
            classmap
                .entry(class.clone())
                .or_insert_with(|| path.clone());
        }
    }
    let sort_ms = sort_start.elapsed().as_millis();

    let gen_start = std::time::Instant::now();
    let classmap_count = classmap.len();

    let base_real =
        fs::canonicalize(&config.project_dir).unwrap_or_else(|_| PathBuf::from(&config.project_dir));
    let base_str = base_real.to_string_lossy().to_string();

    let classmap_file_content = generate_classmap_file(&classmap, &vendor_str, &base_str);
    let psr4_file_content = generate_psr4_file(&config.autoload.psr4, &vendor_str, &base_str);
    let namespaces_file_content =
        generate_namespaces_file(&config.autoload.psr0, &vendor_str, &base_str);
    let files_file_content = generate_files_file(&config.autoload.files, &vendor_str, &base_str);

    let static_file_content = if let Some(ref sfx) = config.suffix {
        let td = config.target_dir.as_deref().unwrap_or("");
        let td_real = if !td.is_empty() {
            fs::canonicalize(td)
                .unwrap_or_else(|_| PathBuf::from(td))
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        };
        generate_static_file(
            sfx,
            &config.autoload.psr4,
            &config.autoload.psr0,
            &classmap,
            &config.autoload.files,
            &vendor_str,
            &base_str,
            &td_real,
        )
    } else {
        String::new()
    };

    // Generate autoload.php and autoload_real.php when we have a suffix
    let autoload_php_content = config
        .suffix
        .as_ref()
        .map(|sfx| generate_autoload_php(sfx));
    let autoload_real_php_content = config.suffix.as_ref().map(|sfx| {
        generate_autoload_real_php(sfx, config.has_platform_check, config.has_files_autoload)
    });

    let generate_ms = gen_start.elapsed().as_millis();

    // Determine whether we write files directly or return contents via JSON.
    // With staging_suffix, files are written with a suffix appended (e.g. ".turbo")
    // so PHP can atomically rename them after parent::dump completes.
    let use_staging = config.staging_suffix.is_some();
    let suffix_ext = config.staging_suffix.as_deref().unwrap_or("");

    let files_written = if config.write_files || use_staging {
        if let Some(ref td) = config.target_dir {
            let td_path = Path::new(td);
            let vendor_path = Path::new(&config.vendor_dir);
            let write_result = (|| -> Result<(), std::io::Error> {
                fs::write(
                    td_path.join(format!("autoload_classmap.php{suffix_ext}")),
                    &classmap_file_content,
                )?;
                fs::write(
                    td_path.join(format!("autoload_psr4.php{suffix_ext}")),
                    &psr4_file_content,
                )?;
                fs::write(
                    td_path.join(format!("autoload_namespaces.php{suffix_ext}")),
                    &namespaces_file_content,
                )?;

                if !files_file_content.is_empty() {
                    fs::write(
                        td_path.join(format!("autoload_files.php{suffix_ext}")),
                        &files_file_content,
                    )?;
                }

                if !static_file_content.is_empty() {
                    fs::write(
                        td_path.join(format!("autoload_static.php{suffix_ext}")),
                        &static_file_content,
                    )?;
                }

                // Write autoload infrastructure files when suffix is available
                if let Some(ref content) = autoload_php_content {
                    fs::write(
                        vendor_path.join(format!("autoload.php{suffix_ext}")),
                        content,
                    )?;
                }
                if let Some(ref content) = autoload_real_php_content {
                    fs::write(
                        td_path.join(format!("autoload_real.php{suffix_ext}")),
                        content,
                    )?;
                }

                Ok(())
            })();

            if let Err(e) = write_result {
                eprintln!("turbo-rust: failed to write autoload files: {e}");
                std::process::exit(1);
            }

            true
        } else {
            false
        }
    } else {
        false
    };

    if let Some(ref cp) = cache_path {
        save_cache(cp, &walk_result.new_cache);
    }

    // When staging, skip returning file contents — they're already on disk.
    let include_contents = !use_staging;

    let output = Output {
        classmap_count,
        classmap_file_content: if include_contents {
            Some(classmap_file_content)
        } else {
            None
        },
        static_file_content: if include_contents {
            Some(static_file_content)
        } else {
            None
        },
        psr4_file_content: if include_contents {
            Some(psr4_file_content)
        } else {
            None
        },
        namespaces_file_content: if include_contents {
            Some(namespaces_file_content)
        } else {
            None
        },
        files_file_content: if include_contents {
            Some(files_file_content)
        } else {
            None
        },
        files_written,
        stats: Stats {
            files_scanned: walk_result.files_scanned,
            php_files_found: walk_result.php_files_found,
            directories_walked: walk_result.directories_walked,
            cache_hits: walk_result.cache_hits,
            walk_skipped: walk_result.walk_skipped,
            elapsed_ms: start.elapsed().as_millis(),
            walk_ms: walk_parse_ms,
            parse_ms: sort_ms,
            generate_ms,
        },
    };

    serde_json::to_value(output).unwrap()
}

/// Check whether a class should be included in the classmap, applying PSR-4/PSR-0
/// filename compliance filtering to match Composer's `filterByNamespace()` behaviour.
///
/// - Classes in classmap directories are always included.
/// - Classes in PSR-4 directories must have an FQCN that maps to the file's
///   relative path (minus extension) under the base directory.
/// - Classes in PSR-0 directories follow PSR-0 path conventions.
/// - Classes not matched by any mapping are included (conservative fallback).
fn is_class_valid(
    class: &str,
    file_path: &str,
    psr4: &[(String, String)],
    psr0: &[(String, String)],
    classmap_dirs: &[String],
) -> bool {
    // Classmap directories: always include all classes.
    for cm_dir in classmap_dirs {
        let prefix = if cm_dir.ends_with('/') {
            cm_dir.to_string()
        } else {
            format!("{cm_dir}/")
        };
        if file_path.starts_with(&prefix) || file_path == cm_dir.as_str() {
            return true;
        }
    }

    // PSR-4: find the longest (most specific) matching base path.
    let mut best_psr4: Option<(&str, &str)> = None;
    for (ns, base) in psr4 {
        let prefix = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{base}/")
        };
        if file_path.starts_with(&prefix)
            && best_psr4
                .as_ref()
                .is_none_or(|(_, prev_base)| base.len() > prev_base.len())
        {
            best_psr4 = Some((ns.as_str(), base.as_str()));
        }
    }

    if let Some((ns_prefix, base_path)) = best_psr4 {
        return is_psr4_compliant(class, ns_prefix, base_path, file_path);
    }

    // PSR-0: find the longest matching base path.
    let mut best_psr0: Option<(&str, &str)> = None;
    for (ns, base) in psr0 {
        let prefix = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{base}/")
        };
        if file_path.starts_with(&prefix)
            && best_psr0
                .as_ref()
                .is_none_or(|(_, prev_base)| base.len() > prev_base.len())
        {
            best_psr0 = Some((ns.as_str(), base.as_str()));
        }
    }

    if let Some((_, base_path)) = best_psr0 {
        return is_psr0_compliant(class, base_path, file_path);
    }

    // Not in any known mapping — include conservatively.
    true
}

/// PSR-4: class `Foo\Bar\Baz` with prefix `Foo\` and base `/path/to/foo`
/// expects file at `/path/to/foo/Bar/Baz.php`.
///
/// Matches Composer's `filterByNamespace()` which uses positional stripping:
/// `substr($class, strlen($baseNamespace))` — it strips N characters from the
/// FQCN regardless of whether the class actually starts with the prefix.
fn is_psr4_compliant(class: &str, ns_prefix: &str, base_path: &str, file_path: &str) -> bool {
    // Strip .php extension from the relative path
    let sep = if base_path.ends_with('/') { "" } else { "/" };
    let rel_start = base_path.len() + sep.len();
    if file_path.len() <= rel_start {
        return false;
    }
    let relative = &file_path[rel_start..];
    let relative = relative.strip_suffix(".php").unwrap_or(relative);

    // Positional strip: remove N characters where N = namespace prefix length.
    // This matches Composer's `substr($class, strlen($baseNamespace))`.
    let prefix_len = ns_prefix.len();
    let sub_class = if prefix_len > 0 && class.len() > prefix_len {
        &class[prefix_len..]
    } else if prefix_len == 0 {
        class
    } else {
        return false;
    };

    // Convert namespace separators to path separators
    let expected = sub_class.replace('\\', "/");
    expected == relative
}

/// PSR-0: class `Foo\Bar_Baz` with base `/path/to/lib` expects file at
/// `/path/to/lib/Foo/Bar/Baz.php` (namespace `\` → `/`, classname `_` → `/`).
fn is_psr0_compliant(class: &str, base_path: &str, file_path: &str) -> bool {
    let sep = if base_path.ends_with('/') { "" } else { "/" };
    let rel_start = base_path.len() + sep.len();
    if file_path.len() <= rel_start {
        return false;
    }
    let relative = &file_path[rel_start..];
    let relative = relative.strip_suffix(".php").unwrap_or(relative);

    // PSR-0: split at last backslash
    let expected = if let Some(last_bs) = class.rfind('\\') {
        let namespace_part = &class[..last_bs + 1]; // includes trailing backslash
        let class_name = &class[last_bs + 1..];
        let ns_path = namespace_part.replace('\\', "/");
        let cls_path = class_name.replace('_', "/");
        format!("{ns_path}{cls_path}")
    } else {
        class.replace('_', "/")
    };

    expected == relative
}

#[cfg(test)]
mod tests {
    use super::*;
    use cache::CACHE_VERSION;
    use std::io::Write;
    use tempfile::TempDir;

    fn test_config(
        project_dir: String,
        vendor_dir: String,
        autoload: AutoloadMappings,
        exclude_from_classmap: Vec<String>,
        target_dir: Option<String>,
        suffix: Option<String>,
        write_files: bool,
    ) -> ClassmapConfig {
        ClassmapConfig {
            project_dir,
            vendor_dir,
            autoload,
            exclude_from_classmap,
            target_dir,
            suffix,
            write_files,
            staging_suffix: None,
            has_platform_check: false,
            has_files_autoload: false,
        }
    }

    #[test]
    fn run_with_real_files() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let mut f1 = fs::File::create(src_dir.join("Foo.php")).unwrap();
        writeln!(f1, "<?php\nnamespace Acme;\nclass Foo {{}}").unwrap();

        let mut f2 = fs::File::create(src_dir.join("Bar.php")).unwrap();
        writeln!(f2, "<?php\nnamespace Acme;\ninterface Bar {{}}").unwrap();

        let mut f3 = fs::File::create(src_dir.join("helpers.php")).unwrap();
        writeln!(f3, "<?php\nfunction do_stuff() {{}}").unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "Acme\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![],
            None,
            None,
            true,
        ));

        assert_eq!(result["classmap_count"].as_u64().unwrap(), 2);
        let content = result["classmap_file_content"].as_str().unwrap();
        assert!(content.contains("Acme\\\\Foo"));
        assert!(content.contains("Acme\\\\Bar"));
    }

    #[test]
    fn run_with_exclude_pattern() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let tests_dir = src_dir.join("Tests");
        fs::create_dir_all(&tests_dir).unwrap();

        let mut f1 = fs::File::create(src_dir.join("Main.php")).unwrap();
        writeln!(f1, "<?php\nnamespace App;\nclass Main {{}}").unwrap();

        let mut f2 = fs::File::create(tests_dir.join("MainTest.php")).unwrap();
        writeln!(f2, "<?php\nnamespace App\\Tests;\nclass MainTest {{}}").unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        // Build exclude pattern as PHP now does: absolute-path-anchored regex.
        // Use the same path the walker sees (not canonicalized) since on macOS
        // /var and /private/var are different strings.
        let src_str = src_dir.to_string_lossy().to_string();
        let exclude_regex = format!("{}/Tests($|/)", regex::escape(&src_str));

        let result = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![exclude_regex],
            None,
            None,
            true,
        ));

        let content = result["classmap_file_content"].as_str().unwrap();
        assert!(content.contains("App\\\\Main"));
        assert!(!content.contains("App\\\\Tests\\\\MainTest"));
    }

    #[test]
    fn run_with_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![],
            None,
            None,
            true,
        ));

        assert_eq!(result["classmap_count"].as_u64().unwrap(), 0);
    }

    #[test]
    fn run_with_nonexistent_directory() {
        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: "/nonexistent/path/that/does/not/exist".to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result = run(test_config(
            "/tmp".to_string(),
            "/tmp/vendor".to_string(),
            autoload,
            vec![],
            None,
            None,
            true,
        ));

        assert_eq!(result["classmap_count"].as_u64().unwrap(), 0);
    }

    #[test]
    fn warm_cache_skips_walk() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let target_dir = tmp.path().join("composer");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        let mut f1 = fs::File::create(src_dir.join("Foo.php")).unwrap();
        writeln!(f1, "<?php\nnamespace App;\nclass Foo {{}}").unwrap();
        let mut f2 = fs::File::create(src_dir.join("Bar.php")).unwrap();
        writeln!(f2, "<?php\nnamespace App;\nclass Bar {{}}").unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result1 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload.clone(),
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result1["classmap_count"].as_u64().unwrap(), 2);
        assert!(!result1["stats"]["walk_skipped"].as_bool().unwrap());

        let result2 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload.clone(),
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result2["classmap_count"].as_u64().unwrap(), 2);
        assert!(result2["stats"]["walk_skipped"].as_bool().unwrap());
        assert_eq!(result2["stats"]["directories_walked"].as_u64().unwrap(), 0);
    }

    #[test]
    fn warm_cache_detects_new_file() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let target_dir = tmp.path().join("composer");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        let mut f1 = fs::File::create(src_dir.join("Foo.php")).unwrap();
        writeln!(f1, "<?php\nnamespace App;\nclass Foo {{}}").unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result1 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload.clone(),
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result1["classmap_count"].as_u64().unwrap(), 1);

        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut f2 = fs::File::create(src_dir.join("Bar.php")).unwrap();
        writeln!(f2, "<?php\nnamespace App;\nclass Bar {{}}").unwrap();

        let result2 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload.clone(),
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result2["classmap_count"].as_u64().unwrap(), 2);
        assert!(!result2["stats"]["walk_skipped"].as_bool().unwrap());
    }

    #[test]
    fn warm_cache_detects_file_content_change() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let target_dir = tmp.path().join("composer");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        let foo_path = src_dir.join("Foo.php");
        fs::write(&foo_path, "<?php\nnamespace App;\nclass Foo {}\n").unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result1 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload.clone(),
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result1["classmap_count"].as_u64().unwrap(), 1);
        assert_eq!(result1["stats"]["cache_hits"].as_u64().unwrap(), 0);

        std::thread::sleep(std::time::Duration::from_secs(1));
        // Rewrite Foo.php with changed content (class renamed) and add a new
        // PSR-4-compliant file to verify cache detects the mtime change.
        fs::write(
            &foo_path,
            "<?php\nnamespace App;\nclass Foo { public function changed(): void {} }\n",
        )
        .unwrap();
        fs::write(
            src_dir.join("FooExtra.php"),
            "<?php\nnamespace App;\nclass FooExtra {}\n",
        )
        .unwrap();

        let result2 = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));
        assert_eq!(result2["classmap_count"].as_u64().unwrap(), 2);
    }

    #[test]
    fn cache_format_v2_includes_dir_mtimes() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let target_dir = tmp.path().join("composer");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        fs::write(
            src_dir.join("Foo.php"),
            "<?php\nnamespace App;\nclass Foo {}\n",
        )
        .unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let _ = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![],
            Some(target_dir.to_string_lossy().to_string()),
            None,
            true,
        ));

        let cache_path = target_dir.join(".turbo-cache");
        assert!(cache_path.exists());
        let data: serde_json::Value =
            serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        assert_eq!(data["version"].as_u64().unwrap(), CACHE_VERSION as u64);
        assert!(data["files"].is_object());
        assert!(data["dir_mtimes"].is_object());
        assert!(!data["dir_mtimes"].as_object().unwrap().is_empty());
    }

    #[test]
    fn staging_suffix_writes_with_suffix_and_omits_contents() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let target_dir = tmp.path().join("composer");
        let vendor_dir = tmp.path().join("vendor");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&vendor_dir).unwrap();

        fs::write(
            src_dir.join("Foo.php"),
            "<?php\nnamespace App;\nclass Foo {}\n",
        )
        .unwrap();

        let result = run(ClassmapConfig {
            project_dir: tmp.path().to_string_lossy().to_string(),
            vendor_dir: vendor_dir.to_string_lossy().to_string(),
            autoload: AutoloadMappings {
                psr4: vec![NamespaceMapping {
                    namespace: "App\\".to_string(),
                    path: src_dir.to_string_lossy().to_string(),
                }],
                psr0: vec![],
                classmap: vec![],
                files: vec![],
            },
            exclude_from_classmap: vec![],
            target_dir: Some(target_dir.to_string_lossy().to_string()),
            suffix: Some("test123".to_string()),
            write_files: false,
            staging_suffix: Some(".turbo".to_string()),
            has_platform_check: true,
            has_files_autoload: false,
        });

        // File contents should NOT be in the JSON response
        assert!(result.get("classmap_file_content").is_none());
        assert!(result.get("static_file_content").is_none());
        assert!(result["files_written"].as_bool().unwrap());
        assert_eq!(result["classmap_count"].as_u64().unwrap(), 1);

        // Staged files should exist on disk
        assert!(target_dir.join("autoload_classmap.php.turbo").exists());
        assert!(target_dir.join("autoload_psr4.php.turbo").exists());
        assert!(target_dir.join("autoload_static.php.turbo").exists());
        assert!(target_dir.join("autoload_real.php.turbo").exists());
        assert!(vendor_dir.join("autoload.php.turbo").exists());

        // Verify autoload.php content
        let autoload_content = fs::read_to_string(vendor_dir.join("autoload.php.turbo")).unwrap();
        assert!(autoload_content.contains("ComposerAutoloaderInittest123"));

        // Verify autoload_real.php content
        let real_content = fs::read_to_string(target_dir.join("autoload_real.php.turbo")).unwrap();
        assert!(real_content.contains("ComposerAutoloaderInittest123"));
        assert!(real_content.contains("platform_check.php"));
    }

    #[test]
    fn psr4_compliant_class_matches_filename() {
        assert!(is_psr4_compliant(
            "App\\Models\\User",
            "App\\",
            "/project/src",
            "/project/src/Models/User.php",
        ));
    }

    #[test]
    fn psr4_rejects_class_in_wrong_file() {
        // LazyValue defined in PhpFilesAdapter.php — PSR-4 non-compliant
        assert!(!is_psr4_compliant(
            "Symfony\\Component\\Cache\\Adapter\\LazyValue",
            "Symfony\\Component\\Cache\\",
            "/vendor/symfony/cache",
            "/vendor/symfony/cache/Adapter/PhpFilesAdapter.php",
        ));
    }

    #[test]
    fn psr4_primary_class_in_correct_file() {
        assert!(is_psr4_compliant(
            "Symfony\\Component\\Cache\\Adapter\\PhpFilesAdapter",
            "Symfony\\Component\\Cache\\",
            "/vendor/symfony/cache",
            "/vendor/symfony/cache/Adapter/PhpFilesAdapter.php",
        ));
    }

    #[test]
    fn psr4_empty_namespace_prefix() {
        assert!(is_psr4_compliant(
            "GlobalClass",
            "",
            "/project/lib",
            "/project/lib/GlobalClass.php",
        ));
    }

    #[test]
    fn psr4_positional_strip_matches_composer_behaviour() {
        // Composer uses positional stripping: substr($class, strlen($ns_prefix)).
        // If the prefix length happens to produce a matching path suffix, the class
        // is accepted — even if the class doesn't actually start with the prefix.
        // e.g. "Src\TestClass1" with prefix "App\" (both 4 chars) strips to "TestClass1"
        assert!(is_psr4_compliant(
            "Src\\TestClass1",
            "App\\",
            "/project/src",
            "/project/src/TestClass1.php",
        ));
    }

    #[test]
    fn psr4_rejects_when_positional_strip_produces_wrong_path() {
        // "Other\\Foo" with prefix "App\\" strips 4 chars → "r\\Foo" → path "r/Foo"
        // File is at "/project/src/Foo.php" → relative is "Foo" — doesn't match "r/Foo"
        assert!(!is_psr4_compliant(
            "Other\\Foo",
            "App\\",
            "/project/src",
            "/project/src/Foo.php",
        ));
    }

    #[test]
    fn psr0_compliant_class() {
        assert!(is_psr0_compliant(
            "Psr\\Log\\LoggerInterface",
            "/vendor/psr/log",
            "/vendor/psr/log/Psr/Log/LoggerInterface.php",
        ));
    }

    #[test]
    fn psr0_underscore_to_path() {
        // PSR-0: underscores in the class name (after last \) become /
        assert!(is_psr0_compliant(
            "Twig_Extension_Core",
            "/vendor/twig/twig/lib",
            "/vendor/twig/twig/lib/Twig/Extension/Core.php",
        ));
    }

    #[test]
    fn psr0_rejects_class_in_wrong_file() {
        assert!(!is_psr0_compliant(
            "Psr\\Log\\ExtraClass",
            "/vendor/psr/log",
            "/vendor/psr/log/Psr/Log/LoggerInterface.php",
        ));
    }

    #[test]
    fn is_class_valid_classmap_always_includes() {
        // A class in a classmap directory is always included, even if PSR-4 non-compliant
        let psr4 = vec![("App\\".to_string(), "/project/src".to_string())];
        let classmap = vec!["/project/src".to_string()];
        assert!(is_class_valid(
            "App\\SecondaryClass",
            "/project/src/MainClass.php",
            &psr4,
            &[],
            &classmap,
        ));
    }

    #[test]
    fn is_class_valid_psr4_filters_secondary_classes() {
        let psr4 = vec![("App\\".to_string(), "/project/src".to_string())];
        // Primary class: matches filename
        assert!(is_class_valid(
            "App\\MainClass",
            "/project/src/MainClass.php",
            &psr4,
            &[],
            &[],
        ));
        // Secondary class: doesn't match filename — should be rejected
        assert!(!is_class_valid(
            "App\\SecondaryClass",
            "/project/src/MainClass.php",
            &psr4,
            &[],
            &[],
        ));
    }

    #[test]
    fn psr4_filters_secondary_classes_in_real_scenario() {
        // Simulates the real Symfony case: LazyValue in PhpFilesAdapter.php
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let adapter_dir = src_dir.join("Adapter");
        fs::create_dir_all(&adapter_dir).unwrap();

        let mut f = fs::File::create(adapter_dir.join("PhpFilesAdapter.php")).unwrap();
        writeln!(
            f,
            "<?php\nnamespace App\\Adapter;\nclass PhpFilesAdapter {{}}\nclass LazyValue {{}}"
        )
        .unwrap();

        let autoload = AutoloadMappings {
            psr4: vec![NamespaceMapping {
                namespace: "App\\".to_string(),
                path: src_dir.to_string_lossy().to_string(),
            }],
            psr0: vec![],
            classmap: vec![],
            files: vec![],
        };

        let result = run(test_config(
            tmp.path().to_string_lossy().to_string(),
            tmp.path().join("vendor").to_string_lossy().to_string(),
            autoload,
            vec![],
            None,
            None,
            true,
        ));

        let content = result["classmap_file_content"].as_str().unwrap();
        // PhpFilesAdapter matches PSR-4 (class name = filename)
        assert!(content.contains("App\\\\Adapter\\\\PhpFilesAdapter"));
        // LazyValue does NOT match PSR-4 (class name != filename)
        assert!(
            !content.contains("App\\\\Adapter\\\\LazyValue"),
            "LazyValue should be excluded by PSR-4 compliance check"
        );
    }
}
