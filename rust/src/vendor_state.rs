use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Deserialize)]
pub struct PackageCheck {
    pub name: String,
    pub install_path: String,
}

#[derive(Debug, Serialize)]
struct VendorStateResult {
    present: usize,
    missing: Vec<String>,
    incomplete: Vec<String>,
    total: usize,
    elapsed_ms: u128,
}

pub fn run(packages: Vec<PackageCheck>) -> serde_json::Value {
    let start = std::time::Instant::now();
    let total = packages.len();
    let present = AtomicUsize::new(0);

    let results: Vec<(Option<String>, Option<String>)> = packages
        .par_iter()
        .map(|pkg| {
            let path = Path::new(&pkg.install_path);
            if !path.exists() {
                return (Some(pkg.name.clone()), None);
            }

            // A package is "present" if its directory has at least one entry
            let has_content = if path.is_file() {
                true
            } else {
                fs::read_dir(path)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false)
            };

            if has_content {
                present.fetch_add(1, Ordering::Relaxed);
                (None, None)
            } else {
                (None, Some(pkg.name.clone()))
            }
        })
        .collect();

    let mut missing = Vec::new();
    let mut incomplete = Vec::new();
    for (m, i) in results {
        if let Some(name) = m {
            missing.push(name);
        }
        if let Some(name) = i {
            incomplete.push(name);
        }
    }

    missing.sort();
    incomplete.sort();

    let result = VendorStateResult {
        present: present.load(Ordering::Relaxed),
        missing,
        incomplete,
        total,
        elapsed_ms: start.elapsed().as_millis(),
    };

    serde_json::to_value(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn all_packages_present() {
        let tmp = TempDir::new().unwrap();

        let pkgs: Vec<_> = (0..5)
            .map(|i| {
                let dir = tmp.path().join(format!("vendor-{i}"));
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("composer.json"), b"{}").unwrap();
                PackageCheck {
                    name: format!("vendor/pkg-{i}"),
                    install_path: dir.to_string_lossy().to_string(),
                }
            })
            .collect();

        let result = run(pkgs);
        assert_eq!(result["present"].as_u64().unwrap(), 5);
        assert_eq!(result["total"].as_u64().unwrap(), 5);
        assert!(result["missing"].as_array().unwrap().is_empty());
        assert!(result["incomplete"].as_array().unwrap().is_empty());
    }

    #[test]
    fn missing_packages_detected() {
        let pkgs = vec![
            PackageCheck {
                name: "vendor/missing-1".to_string(),
                install_path: "/nonexistent/path/1".to_string(),
            },
            PackageCheck {
                name: "vendor/missing-2".to_string(),
                install_path: "/nonexistent/path/2".to_string(),
            },
        ];

        let result = run(pkgs);
        assert_eq!(result["present"].as_u64().unwrap(), 0);
        assert_eq!(result["total"].as_u64().unwrap(), 2);
        let missing = result["missing"].as_array().unwrap();
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn incomplete_packages_detected() {
        let tmp = TempDir::new().unwrap();
        let empty_dir = tmp.path().join("empty-pkg");
        fs::create_dir_all(&empty_dir).unwrap();

        let pkgs = vec![PackageCheck {
            name: "vendor/empty".to_string(),
            install_path: empty_dir.to_string_lossy().to_string(),
        }];

        let result = run(pkgs);
        assert_eq!(result["present"].as_u64().unwrap(), 0);
        assert_eq!(result["incomplete"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn mixed_present_missing_incomplete() {
        let tmp = TempDir::new().unwrap();

        let good_dir = tmp.path().join("good");
        fs::create_dir_all(&good_dir).unwrap();
        fs::write(good_dir.join("file.php"), b"<?php").unwrap();

        let empty_dir = tmp.path().join("empty");
        fs::create_dir_all(&empty_dir).unwrap();

        let pkgs = vec![
            PackageCheck {
                name: "vendor/good".to_string(),
                install_path: good_dir.to_string_lossy().to_string(),
            },
            PackageCheck {
                name: "vendor/missing".to_string(),
                install_path: "/nonexistent/dir".to_string(),
            },
            PackageCheck {
                name: "vendor/empty".to_string(),
                install_path: empty_dir.to_string_lossy().to_string(),
            },
        ];

        let result = run(pkgs);
        assert_eq!(result["present"].as_u64().unwrap(), 1);
        assert_eq!(result["total"].as_u64().unwrap(), 3);
        assert_eq!(result["missing"].as_array().unwrap().len(), 1);
        assert_eq!(result["incomplete"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn empty_package_list() {
        let result = run(vec![]);
        assert_eq!(result["present"].as_u64().unwrap(), 0);
        assert_eq!(result["total"].as_u64().unwrap(), 0);
    }

    #[test]
    fn many_packages_parallel() {
        let tmp = TempDir::new().unwrap();

        let pkgs: Vec<_> = (0..100)
            .map(|i| {
                let dir = tmp.path().join(format!("pkg-{i}"));
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("autoload.php"), b"<?php").unwrap();
                PackageCheck {
                    name: format!("vendor/pkg-{i}"),
                    install_path: dir.to_string_lossy().to_string(),
                }
            })
            .collect();

        let result = run(pkgs);
        assert_eq!(result["present"].as_u64().unwrap(), 100);
        assert_eq!(result["total"].as_u64().unwrap(), 100);
    }
}
