use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Deserialize)]
pub struct PackageExtraction {
    pub zip: String,
    pub dest: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
struct ExtractionResult {
    extracted: usize,
    failed: Vec<FailedExtraction>,
    total_files: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct FailedExtraction {
    name: String,
    error: String,
}

pub fn run(packages: Vec<PackageExtraction>) -> serde_json::Value {
    let start = std::time::Instant::now();
    let total_files = AtomicUsize::new(0);
    let extracted = AtomicUsize::new(0);

    let failed: Vec<FailedExtraction> = packages
        .par_iter()
        .filter_map(|pkg| match extract_one(pkg, &total_files) {
            Ok(()) => {
                extracted.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(e) => Some(FailedExtraction {
                name: pkg.name.clone(),
                error: e.to_string(),
            }),
        })
        .collect();

    let result = ExtractionResult {
        extracted: extracted.load(Ordering::Relaxed),
        failed,
        total_files: total_files.load(Ordering::Relaxed),
        elapsed_ms: start.elapsed().as_millis(),
    };

    serde_json::to_value(result).unwrap()
}

fn extract_one(
    pkg: &PackageExtraction,
    total_files: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let zip_path = Path::new(&pkg.zip);
    let dest = Path::new(&pkg.dest);

    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;

    match zip_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "zip" => extract_zip(zip_path, dest, total_files),
        "gz" | "tgz" => extract_tar_gz(zip_path, dest, total_files),
        "tar" => extract_tar(zip_path, dest, total_files),
        other => Err(format!("unsupported archive format: {other}").into()),
    }
}

fn extract_zip(
    zip_path: &Path,
    dest: &Path,
    total_files: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(zip_path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let reader = Cursor::new(&mmap[..]);
    let mut archive = zip::ZipArchive::new(reader)?;

    let count = archive.len();
    total_files.fetch_add(count, Ordering::Relaxed);

    let strip = detect_strip_prefix(&mut archive);

    let mut file_entries: Vec<String> = Vec::with_capacity(count);
    let mut dirs_to_create: Vec<String> = Vec::new();

    for i in 0..count {
        let entry = archive.by_index_raw(i)?;
        let raw_name = entry.name().to_string();

        let relative = match &strip {
            Some(prefix) => match raw_name.strip_prefix(prefix.as_str()) {
                Some(rest) => rest.to_string(),
                None => raw_name,
            },
            None => raw_name,
        };

        if relative.is_empty() {
            continue;
        }

        if relative.ends_with('/') {
            dirs_to_create.push(relative);
            continue;
        }

        if let Some(parent) = Path::new(&relative).parent() {
            let parent_str = parent.to_string_lossy().to_string();
            if !parent_str.is_empty() {
                dirs_to_create.push(format!("{parent_str}/"));
            }
        }

        file_entries.push(relative);
    }

    dirs_to_create.sort();
    dirs_to_create.dedup();
    for dir in &dirs_to_create {
        fs::create_dir_all(dest.join(dir))?;
    }

    let mmap_ref: &[u8] = &mmap;

    file_entries.par_iter().try_for_each(|relative| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let out_path = dest.join(relative);

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let full_name = match &strip {
            Some(prefix) => format!("{prefix}{relative}"),
            None => relative.clone(),
        };

        let cursor = Cursor::new(mmap_ref);
        let mut arch = zip::ZipArchive::new(cursor)?;
        let mut zip_entry = arch.by_name(&full_name)?;
        let mut outfile = fs::File::create(&out_path)?;
        std::io::copy(&mut zip_entry, &mut outfile)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = zip_entry.unix_mode() {
                fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))?;
            }
        }

        Ok(())
    })?;

    Ok(())
}

fn detect_strip_prefix(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> Option<String> {
    let mut common: Option<String> = None;

    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(i) else {
            return None;
        };
        let name = entry.name();

        let first = match name.split('/').next() {
            Some(c) if !c.is_empty() => format!("{c}/"),
            _ => return None,
        };

        match &common {
            None => common = Some(first),
            Some(existing) if *existing != first => return None,
            _ => {}
        }
    }

    common
}

fn extract_tar_gz(
    path: &Path,
    dest: &Path,
    total_files: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    extract_tar_archive(decoder, dest, total_files)
}

fn extract_tar(
    path: &Path,
    dest: &Path,
    total_files: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(path)?;
    extract_tar_archive(file, dest, total_files)
}

fn extract_tar_archive<R: std::io::Read>(
    reader: R,
    dest: &Path,
    total_files: &AtomicUsize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut archive = tar::Archive::new(reader);

    let mut count = 0usize;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.to_path_buf();
        let out = dest.join(&entry_path);

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(&out)?;
            count += 1;
        }
    }

    total_files.fetch_add(count, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_zip(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> String {
        let zip_path = dir.join(name);
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        for (entry_name, content) in files {
            zip_writer.start_file(*entry_name, options).unwrap();
            zip_writer.write_all(content).unwrap();
        }

        zip_writer.finish().unwrap();
        zip_path.to_string_lossy().to_string()
    }

    fn create_test_tar(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> String {
        let tar_path = dir.join(name);
        let file = fs::File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(file);

        for (entry_name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, *entry_name, &content[..])
                .unwrap();
        }

        builder.finish().unwrap();
        tar_path.to_string_lossy().to_string()
    }

    fn create_test_tar_gz(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> String {
        let tar_gz_path = dir.join(name);
        let file = fs::File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        for (entry_name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, *entry_name, &content[..])
                .unwrap();
        }

        builder.into_inner().unwrap().finish().unwrap();
        tar_gz_path.to_string_lossy().to_string()
    }

    #[test]
    fn extract_zip_basic() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();
        let dest_dir = tmp.path().join("output");

        let zip_path = create_test_zip(
            &archives_dir,
            "test.zip",
            &[
                ("hello.txt", b"Hello World"),
                ("sub/nested.txt", b"Nested content"),
            ],
        );

        let packages = vec![PackageExtraction {
            zip: zip_path,
            dest: dest_dir.to_string_lossy().to_string(),
            name: "test/package".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);
        assert_eq!(result["total_files"].as_u64().unwrap(), 2);
        assert!(result["failed"].as_array().unwrap().is_empty());

        assert_eq!(
            fs::read_to_string(dest_dir.join("hello.txt")).unwrap(),
            "Hello World"
        );
        assert_eq!(
            fs::read_to_string(dest_dir.join("sub/nested.txt")).unwrap(),
            "Nested content"
        );
    }

    #[test]
    fn extract_zip_with_strip_prefix() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();
        let dest_dir = tmp.path().join("output");

        let zip_path = create_test_zip(
            &archives_dir,
            "prefixed.zip",
            &[
                ("vendor-pkg-abc123/src/Foo.php", b"<?php class Foo {}"),
                ("vendor-pkg-abc123/README.md", b"# Hello"),
            ],
        );

        let packages = vec![PackageExtraction {
            zip: zip_path,
            dest: dest_dir.to_string_lossy().to_string(),
            name: "vendor/pkg".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);

        assert_eq!(
            fs::read_to_string(dest_dir.join("src/Foo.php")).unwrap(),
            "<?php class Foo {}"
        );
        assert_eq!(
            fs::read_to_string(dest_dir.join("README.md")).unwrap(),
            "# Hello"
        );
    }

    #[test]
    fn extract_tar_basic() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();
        let dest_dir = tmp.path().join("output");

        let tar_path = create_test_tar(
            &archives_dir,
            "test.tar",
            &[
                ("file1.txt", b"content one"),
                ("dir/file2.txt", b"content two"),
            ],
        );

        let packages = vec![PackageExtraction {
            zip: tar_path,
            dest: dest_dir.to_string_lossy().to_string(),
            name: "test/tar-pkg".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);
        assert_eq!(result["total_files"].as_u64().unwrap(), 2);

        assert_eq!(
            fs::read_to_string(dest_dir.join("file1.txt")).unwrap(),
            "content one"
        );
        assert_eq!(
            fs::read_to_string(dest_dir.join("dir/file2.txt")).unwrap(),
            "content two"
        );
    }

    #[test]
    fn extract_tar_gz_basic() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();
        let dest_dir = tmp.path().join("output");

        let tar_gz_path = create_test_tar_gz(
            &archives_dir,
            "test.tar.gz",
            &[
                ("a.txt", b"aaa"),
                ("b.txt", b"bbb"),
                ("c/d.txt", b"ccc"),
            ],
        );

        let packages = vec![PackageExtraction {
            zip: tar_gz_path,
            dest: dest_dir.to_string_lossy().to_string(),
            name: "test/targz-pkg".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);
        assert_eq!(result["total_files"].as_u64().unwrap(), 3);

        assert_eq!(fs::read_to_string(dest_dir.join("a.txt")).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(dest_dir.join("b.txt")).unwrap(), "bbb");
        assert_eq!(
            fs::read_to_string(dest_dir.join("c/d.txt")).unwrap(),
            "ccc"
        );
    }

    #[test]
    fn extract_multiple_packages() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();

        let zip1 = create_test_zip(
            &archives_dir,
            "pkg1.zip",
            &[
                ("pkg1-abc123/src/A.php", b"<?php class A {}"),
                ("pkg1-abc123/composer.json", b"{}"),
            ],
        );
        let zip2 = create_test_zip(
            &archives_dir,
            "pkg2.zip",
            &[
                ("pkg2-def456/src/B.php", b"<?php class B {}"),
                ("pkg2-def456/composer.json", b"{}"),
            ],
        );

        let dest1 = tmp.path().join("pkg1");
        let dest2 = tmp.path().join("pkg2");

        let packages = vec![
            PackageExtraction {
                zip: zip1,
                dest: dest1.to_string_lossy().to_string(),
                name: "vendor/pkg1".to_string(),
            },
            PackageExtraction {
                zip: zip2,
                dest: dest2.to_string_lossy().to_string(),
                name: "vendor/pkg2".to_string(),
            },
        ];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 2);
        assert!(result["failed"].as_array().unwrap().is_empty());

        assert!(dest1.join("src/A.php").exists());
        assert!(dest2.join("src/B.php").exists());
    }

    #[test]
    fn extract_nonexistent_archive_reports_failure() {
        let tmp = TempDir::new().unwrap();
        let dest_dir = tmp.path().join("output");

        let packages = vec![PackageExtraction {
            zip: "/nonexistent/archive.zip".to_string(),
            dest: dest_dir.to_string_lossy().to_string(),
            name: "broken/pkg".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 0);

        let failed = result["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["name"].as_str().unwrap(), "broken/pkg");
    }

    #[test]
    fn extract_unsupported_format_reports_failure() {
        let tmp = TempDir::new().unwrap();
        let bad_file = tmp.path().join("archive.rar");
        fs::write(&bad_file, b"not a real archive").unwrap();

        let dest_dir = tmp.path().join("output");

        let packages = vec![PackageExtraction {
            zip: bad_file.to_string_lossy().to_string(),
            dest: dest_dir.to_string_lossy().to_string(),
            name: "bad/format".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 0);

        let failed = result["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0]["error"]
            .as_str()
            .unwrap()
            .contains("unsupported archive format"));
    }

    #[test]
    fn extract_overwrites_existing_destination() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();

        let dest_dir = tmp.path().join("output");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("old_file.txt"), "should be removed").unwrap();

        let zip_path = create_test_zip(
            &archives_dir,
            "fresh.zip",
            &[
                ("new_file.txt", b"fresh content"),
                ("other.txt", b"other content"),
            ],
        );

        let packages = vec![PackageExtraction {
            zip: zip_path,
            dest: dest_dir.to_string_lossy().to_string(),
            name: "test/overwrite".to_string(),
        }];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);

        assert!(!dest_dir.join("old_file.txt").exists());
        assert_eq!(
            fs::read_to_string(dest_dir.join("new_file.txt")).unwrap(),
            "fresh content"
        );
    }

    #[test]
    fn extract_empty_packages_list() {
        let result = run(vec![]);
        assert_eq!(result["extracted"].as_u64().unwrap(), 0);
        assert_eq!(result["total_files"].as_u64().unwrap(), 0);
        assert!(result["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn extract_mixed_success_and_failure() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join("archives");
        fs::create_dir_all(&archives_dir).unwrap();

        let good_zip = create_test_zip(
            &archives_dir,
            "good.zip",
            &[
                ("file.txt", b"good content"),
                ("readme.md", b"# Hello"),
            ],
        );

        let good_dest = tmp.path().join("good_out");
        let bad_dest = tmp.path().join("bad_out");

        let packages = vec![
            PackageExtraction {
                zip: good_zip,
                dest: good_dest.to_string_lossy().to_string(),
                name: "good/pkg".to_string(),
            },
            PackageExtraction {
                zip: "/nonexistent.zip".to_string(),
                dest: bad_dest.to_string_lossy().to_string(),
                name: "bad/pkg".to_string(),
            },
        ];

        let result = run(packages);
        assert_eq!(result["extracted"].as_u64().unwrap(), 1);

        let failed = result["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["name"].as_str().unwrap(), "bad/pkg");

        assert!(good_dest.join("file.txt").exists());
    }
}
