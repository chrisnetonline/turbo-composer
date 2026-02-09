use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Deserialize)]
pub struct CleanTarget {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
struct CleanResult {
    cleaned: usize,
    failed: Vec<FailedClean>,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct FailedClean {
    name: String,
    error: String,
}

pub fn run(targets: Vec<CleanTarget>) -> serde_json::Value {
    let start = std::time::Instant::now();
    let cleaned = AtomicUsize::new(0);

    let failed: Vec<FailedClean> = targets
        .par_iter()
        .filter_map(|target| {
            let path = Path::new(&target.path);
            if !path.exists() {
                cleaned.fetch_add(1, Ordering::Relaxed);
                return None;
            }

            match fs::remove_dir_all(path) {
                Ok(()) => {
                    cleaned.fetch_add(1, Ordering::Relaxed);
                    None
                }
                Err(e) => Some(FailedClean {
                    name: target.name.clone(),
                    error: e.to_string(),
                }),
            }
        })
        .collect();

    let result = CleanResult {
        cleaned: cleaned.load(Ordering::Relaxed),
        failed,
        elapsed_ms: start.elapsed().as_millis(),
    };

    serde_json::to_value(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn clean_single_directory() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("pkg1");
        fs::create_dir_all(dir.join("src")).unwrap();
        let mut f = fs::File::create(dir.join("src/Foo.php")).unwrap();
        writeln!(f, "<?php class Foo {{}}").unwrap();

        let targets = vec![CleanTarget {
            path: dir.to_string_lossy().to_string(),
            name: "vendor/pkg1".to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["cleaned"].as_u64().unwrap(), 1);
        assert!(result["failed"].as_array().unwrap().is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn clean_multiple_directories() {
        let tmp = TempDir::new().unwrap();
        let dirs: Vec<_> = (0..5)
            .map(|i| {
                let d = tmp.path().join(format!("pkg{i}"));
                fs::create_dir_all(d.join("src")).unwrap();
                fs::write(d.join("src/file.php"), b"<?php").unwrap();
                d
            })
            .collect();

        let targets: Vec<_> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| CleanTarget {
                path: d.to_string_lossy().to_string(),
                name: format!("vendor/pkg{i}"),
            })
            .collect();

        let result = run(targets);
        assert_eq!(result["cleaned"].as_u64().unwrap(), 5);
        assert!(result["failed"].as_array().unwrap().is_empty());
        for d in &dirs {
            assert!(!d.exists());
        }
    }

    #[test]
    fn clean_nonexistent_directory_counts_as_success() {
        let targets = vec![CleanTarget {
            path: "/nonexistent/path/that/does/not/exist".to_string(),
            name: "missing/pkg".to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["cleaned"].as_u64().unwrap(), 1);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn clean_empty_list() {
        let result = run(vec![]);
        assert_eq!(result["cleaned"].as_u64().unwrap(), 0);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn clean_mixed_existing_and_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let existing = tmp.path().join("existing");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("file.txt"), b"content").unwrap();

        let targets = vec![
            CleanTarget {
                path: existing.to_string_lossy().to_string(),
                name: "real/pkg".to_string(),
            },
            CleanTarget {
                path: "/nonexistent/dir".to_string(),
                name: "fake/pkg".to_string(),
            },
        ];

        let result = run(targets);
        assert_eq!(result["cleaned"].as_u64().unwrap(), 2);
        assert!(result["failed"].as_array().unwrap().is_empty());
        assert!(!existing.exists());
    }
}
