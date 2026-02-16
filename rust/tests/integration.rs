use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn cargo_bin() -> String {
    let output = Command::new("cargo")
        .args(["build", "--manifest-path", "rust/Cargo.toml"])
        .current_dir(env!("CARGO_MANIFEST_DIR").to_string() + "/..")
        .output()
        .expect("cargo build failed");
    if !output.status.success() {
        panic!(
            "cargo build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let target_dir = format!("{}/target/debug/turbo-composer", env!("CARGO_MANIFEST_DIR"));
    target_dir
}

fn run_binary(input_json: &str) -> serde_json::Value {
    let bin = cargo_bin();
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input_json.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "binary exited with error: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("invalid JSON output from binary")
}

#[test]
fn version_flag() {
    let bin = cargo_bin();
    let output = Command::new(&bin)
        .arg("--version")
        .output()
        .expect("failed to run binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("turbo-composer"),
        "version output should contain 'turbo-composer', got: {stdout}"
    );
    assert!(
        stdout.contains("0.1.0"),
        "version output should contain version number, got: {stdout}"
    );
}

#[test]
fn classmap_command_via_stdin() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();

    let mut f = fs::File::create(src_dir.join("User.php")).unwrap();
    writeln!(f, "<?php\nnamespace App\\Models;\n\nclass User {{}}").unwrap();

    let mut f = fs::File::create(src_dir.join("Post.php")).unwrap();
    writeln!(f, "<?php\nnamespace App\\Models;\n\nclass Post {{}}").unwrap();

    let input = serde_json::json!({
        "command": "classmap",
        "project_dir": tmp.path().to_string_lossy(),
        "vendor_dir": tmp.path().join("vendor").to_string_lossy(),
        "autoload": {
            "psr-4": [{
                "namespace": "App\\Models\\",
                "path": src_dir.to_string_lossy()
            }],
            "psr-0": [],
            "classmap": []
        },
        "exclude_from_classmap": []
    });

    let result = run_binary(&input.to_string());

    assert_eq!(result["classmap_count"].as_u64().unwrap(), 2);
    let content = result["classmap_file_content"].as_str().unwrap();
    assert!(
        content.contains("App\\\\Models\\\\User"),
        "missing App\\Models\\User in classmap content"
    );
    assert!(
        content.contains("App\\\\Models\\\\Post"),
        "missing App\\Models\\Post in classmap content"
    );

    let stats = &result["stats"];
    assert_eq!(stats["files_scanned"].as_u64().unwrap(), 2);
}

#[test]
fn extract_command_via_stdin() {
    let tmp = TempDir::new().unwrap();
    let archives_dir = tmp.path().join("archives");
    fs::create_dir_all(&archives_dir).unwrap();

    // Create a zip archive with a common prefix directory (like real Composer packages)
    let zip_path = archives_dir.join("pkg.zip");
    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip_writer
            .start_file("test-pkg-abc123/hello.txt", options)
            .unwrap();
        zip_writer.write_all(b"Hello from zip").unwrap();
        zip_writer
            .start_file("test-pkg-abc123/composer.json", options)
            .unwrap();
        zip_writer.write_all(b"{}").unwrap();
        zip_writer.finish().unwrap();
    }

    let dest_dir = tmp.path().join("extracted");

    let input = serde_json::json!({
        "command": "extract",
        "packages": [{
            "zip": zip_path.to_string_lossy(),
            "dest": dest_dir.to_string_lossy(),
            "name": "test/pkg"
        }]
    });

    let result = run_binary(&input.to_string());
    assert_eq!(result["extracted"].as_u64().unwrap(), 1);
    assert!(result["failed"].as_array().unwrap().is_empty());

    // Prefix "test-pkg-abc123/" is stripped
    assert_eq!(
        fs::read_to_string(dest_dir.join("hello.txt")).unwrap(),
        "Hello from zip"
    );
}

#[test]
fn classmap_with_excludes_via_stdin() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let test_dir = src_dir.join("Tests");
    fs::create_dir_all(&test_dir).unwrap();

    let mut f = fs::File::create(src_dir.join("App.php")).unwrap();
    writeln!(f, "<?php\nnamespace App;\nclass App {{}}").unwrap();

    let mut f = fs::File::create(test_dir.join("AppTest.php")).unwrap();
    writeln!(f, "<?php\nnamespace App\\Tests;\nclass AppTest {{}}").unwrap();

    let input = serde_json::json!({
        "command": "classmap",
        "project_dir": tmp.path().to_string_lossy(),
        "vendor_dir": tmp.path().join("vendor").to_string_lossy(),
        "autoload": {
            "psr-4": [{
                "namespace": "App\\",
                "path": src_dir.to_string_lossy()
            }],
            "psr-0": [],
            "classmap": []
        },
        "exclude_from_classmap": ["*Tests*"]
    });

    let result = run_binary(&input.to_string());
    let content = result["classmap_file_content"].as_str().unwrap();

    assert!(content.contains("App\\\\App"), "missing App\\App");
    assert!(
        !content.contains("App\\\\Tests\\\\AppTest"),
        "Tests should be excluded"
    );
}

#[test]
fn classmap_empty_input() {
    let input = serde_json::json!({
        "command": "classmap",
        "project_dir": "/nonexistent",
        "vendor_dir": "/nonexistent/vendor",
        "autoload": {
            "psr-4": [],
            "psr-0": [],
            "classmap": []
        },
        "exclude_from_classmap": []
    });

    let result = run_binary(&input.to_string());
    assert_eq!(result["classmap_count"].as_u64().unwrap(), 0);
}

#[test]
fn batch_command_runs_multiple_operations() {
    let tmp = TempDir::new().unwrap();

    // Create directories to clean
    let clean_dir = tmp.path().join("to_clean");
    fs::create_dir_all(&clean_dir).unwrap();
    fs::write(clean_dir.join("file.txt"), "data").unwrap();

    let input = serde_json::json!({
        "command": "batch",
        "operations": [
            {
                "command": "clean",
                "targets": [{
                    "path": clean_dir.to_string_lossy(),
                    "name": "test/clean-pkg"
                }]
            },
            {
                "command": "vendor-check",
                "check_packages": [{
                    "name": "test/missing",
                    "install_path": "/nonexistent/path"
                }]
            }
        ]
    });

    let result = run_binary(&input.to_string());
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);

    // First operation: clean
    assert_eq!(results[0]["command"].as_str().unwrap(), "clean");
    assert_eq!(results[0]["result"]["cleaned"].as_u64().unwrap(), 1);

    // Second operation: vendor-check
    assert_eq!(results[1]["command"].as_str().unwrap(), "vendor-check");
    assert_eq!(results[1]["result"]["total"].as_u64().unwrap(), 1);

    // Verify the directory was actually cleaned
    assert!(!clean_dir.exists());
}

#[test]
fn classmap_staging_writes_files_and_omits_contents() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let target_dir = tmp.path().join("composer");
    let vendor_dir = tmp.path().join("vendor");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&target_dir).unwrap();
    fs::create_dir_all(&vendor_dir).unwrap();

    let mut f = fs::File::create(src_dir.join("Foo.php")).unwrap();
    writeln!(f, "<?php\nnamespace App;\nclass Foo {{}}").unwrap();

    let input = serde_json::json!({
        "command": "classmap",
        "project_dir": tmp.path().to_string_lossy(),
        "vendor_dir": vendor_dir.to_string_lossy(),
        "autoload": {
            "psr-4": [{
                "namespace": "App\\",
                "path": src_dir.to_string_lossy()
            }]
        },
        "exclude_from_classmap": [],
        "target_dir": target_dir.to_string_lossy(),
        "suffix": "testsfx",
        "staging_suffix": ".turbo",
        "has_platform_check": false,
        "has_files_autoload": false
    });

    let result = run_binary(&input.to_string());

    // Contents should not be in JSON response
    assert!(result.get("classmap_file_content").is_none());
    assert!(result.get("static_file_content").is_none());
    assert_eq!(result["classmap_count"].as_u64().unwrap(), 1);
    assert!(result["files_written"].as_bool().unwrap());

    // Staged files should exist
    assert!(target_dir.join("autoload_classmap.php.turbo").exists());
    assert!(target_dir.join("autoload_static.php.turbo").exists());
    assert!(target_dir.join("autoload_real.php.turbo").exists());
    assert!(vendor_dir.join("autoload.php.turbo").exists());

    // Verify generated content quality
    let classmap = fs::read_to_string(target_dir.join("autoload_classmap.php.turbo")).unwrap();
    assert!(classmap.contains("App\\\\Foo"));

    let autoload_real = fs::read_to_string(target_dir.join("autoload_real.php.turbo")).unwrap();
    assert!(autoload_real.contains("ComposerAutoloaderInittestsfx"));
    assert!(!autoload_real.contains("platform_check")); // has_platform_check=false

    let autoload_php = fs::read_to_string(vendor_dir.join("autoload.php.turbo")).unwrap();
    assert!(autoload_php.contains("ComposerAutoloaderInittestsfx"));
}
