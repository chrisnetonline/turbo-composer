use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Deserialize)]
pub struct VerifyTarget {
    pub path: String,
    pub name: String,
    pub algorithm: String,
    pub expected_hash: String,
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    verified: usize,
    failed: Vec<VerifyFailure>,
    total: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct VerifyFailure {
    name: String,
    expected: String,
    actual: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn run(targets: Vec<VerifyTarget>) -> serde_json::Value {
    let start = std::time::Instant::now();
    let total = targets.len();
    let verified = AtomicUsize::new(0);

    let failed: Vec<VerifyFailure> = targets
        .par_iter()
        .filter_map(|target| {
            let path = Path::new(&target.path);

            let file = match fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    return Some(VerifyFailure {
                        name: target.name.clone(),
                        expected: target.expected_hash.clone(),
                        actual: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            };

            let mmap = match unsafe { Mmap::map(&file) } {
                Ok(m) => m,
                Err(e) => {
                    return Some(VerifyFailure {
                        name: target.name.clone(),
                        expected: target.expected_hash.clone(),
                        actual: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            };

            let actual_hash = match target.algorithm.as_str() {
                "sha256" => {
                    let mut hasher = Sha256::new();
                    hasher.update(&mmap[..]);
                    format!("{:x}", hasher.finalize())
                }
                "sha1" => {
                    let mut hasher = Sha1::new();
                    hasher.update(&mmap[..]);
                    format!("{:x}", hasher.finalize())
                }
                other => {
                    return Some(VerifyFailure {
                        name: target.name.clone(),
                        expected: target.expected_hash.clone(),
                        actual: String::new(),
                        error: Some(format!("unsupported algorithm: {other}")),
                    });
                }
            };

            if actual_hash == target.expected_hash {
                verified.fetch_add(1, Ordering::Relaxed);
                None
            } else {
                Some(VerifyFailure {
                    name: target.name.clone(),
                    expected: target.expected_hash.clone(),
                    actual: actual_hash,
                    error: None,
                })
            }
        })
        .collect();

    let result = VerifyResult {
        verified: verified.load(Ordering::Relaxed),
        failed,
        total,
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
    fn verify_sha256_correct() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        let mut f = fs::File::create(&file).unwrap();
        write!(f, "hello world").unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

        let targets = vec![VerifyTarget {
            path: file.to_string_lossy().to_string(),
            name: "test-file".to_string(),
            algorithm: "sha256".to_string(),
            expected_hash: expected.to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 1);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn verify_sha1_correct() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        let mut f = fs::File::create(&file).unwrap();
        write!(f, "hello world").unwrap();

        // SHA1 of "hello world"
        let expected = "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed";

        let targets = vec![VerifyTarget {
            path: file.to_string_lossy().to_string(),
            name: "test-file".to_string(),
            algorithm: "sha1".to_string(),
            expected_hash: expected.to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 1);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn verify_wrong_hash_reports_failure() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, b"hello world").unwrap();

        let targets = vec![VerifyTarget {
            path: file.to_string_lossy().to_string(),
            name: "test-file".to_string(),
            algorithm: "sha256".to_string(),
            expected_hash: "deadbeef".to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 0);
        let failed = result["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["name"].as_str().unwrap(), "test-file");
        assert_eq!(failed[0]["expected"].as_str().unwrap(), "deadbeef");
        assert!(!failed[0]["actual"].as_str().unwrap().is_empty());
    }

    #[test]
    fn verify_missing_file_reports_error() {
        let targets = vec![VerifyTarget {
            path: "/nonexistent/file.zip".to_string(),
            name: "missing-pkg".to_string(),
            algorithm: "sha256".to_string(),
            expected_hash: "abc123".to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 0);
        let failed = result["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0]["error"].as_str().is_some());
    }

    #[test]
    fn verify_unsupported_algorithm() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, b"content").unwrap();

        let targets = vec![VerifyTarget {
            path: file.to_string_lossy().to_string(),
            name: "test-file".to_string(),
            algorithm: "md5".to_string(),
            expected_hash: "abc".to_string(),
        }];

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 0);
        let failed = result["failed"].as_array().unwrap();
        assert!(failed[0]["error"].as_str().unwrap().contains("unsupported"));
    }

    #[test]
    fn verify_multiple_files_parallel() {
        let tmp = TempDir::new().unwrap();
        let mut targets = Vec::new();

        for i in 0..10 {
            let file = tmp.path().join(format!("file{i}.txt"));
            let content = format!("content-{i}");
            fs::write(&file, content.as_bytes()).unwrap();

            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            let hash = format!("{:x}", hasher.finalize());

            targets.push(VerifyTarget {
                path: file.to_string_lossy().to_string(),
                name: format!("pkg-{i}"),
                algorithm: "sha256".to_string(),
                expected_hash: hash,
            });
        }

        let result = run(targets);
        assert_eq!(result["verified"].as_u64().unwrap(), 10);
        assert_eq!(result["total"].as_u64().unwrap(), 10);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn verify_empty_list() {
        let result = run(vec![]);
        assert_eq!(result["verified"].as_u64().unwrap(), 0);
        assert_eq!(result["total"].as_u64().unwrap(), 0);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }
}
