use serde::Deserialize;
use std::io::{self, Read};
use turbo_composer::{classmap, clean, extract, vendor_state, verify};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct Input {
    command: String,

    #[serde(default)]
    packages: Vec<extract::PackageExtraction>,

    #[serde(default)]
    targets: Vec<clean::CleanTarget>,

    #[serde(default)]
    verify_targets: Vec<verify::VerifyTarget>,

    #[serde(default)]
    check_packages: Vec<vendor_state::PackageCheck>,

    #[serde(default)]
    project_dir: Option<String>,
    #[serde(default)]
    vendor_dir: Option<String>,
    #[serde(default)]
    autoload: Option<classmap::AutoloadMappings>,
    #[serde(default)]
    exclude_from_classmap: Vec<String>,
    #[serde(default)]
    target_dir: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default = "default_true")]
    write_files: bool,
}

fn main() {
    let total_start = std::time::Instant::now();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("turbo-composer {}", VERSION);
        return;
    }

    let stdin_start = std::time::Instant::now();
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .expect("failed to read stdin");
    let stdin_ms = stdin_start.elapsed().as_millis();

    let parse_start = std::time::Instant::now();
    let input: Input =
        serde_json::from_str(&buf).expect("failed to parse input JSON");
    let json_parse_ms = parse_start.elapsed().as_millis();

    let command_start = std::time::Instant::now();
    let mut output = match input.command.as_str() {
        "extract" => extract::run(input.packages),
        "clean" => clean::run(input.targets),
        "verify" => verify::run(input.verify_targets),
        "vendor-check" => vendor_state::run(input.check_packages),
        "classmap" => classmap::run(
            input.project_dir.unwrap_or_default(),
            input.vendor_dir.unwrap_or_default(),
            input.autoload.unwrap_or_default(),
            input.exclude_from_classmap,
            input.target_dir,
            input.suffix,
            input.write_files,
        ),
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(1);
        }
    };
    let command_ms = command_start.elapsed().as_millis();

    if let Some(stats) = output.get_mut("stats").and_then(|s| s.as_object_mut()) {
        stats.insert("stdin_read_ms".to_string(), serde_json::json!(stdin_ms));
        stats.insert("json_parse_ms".to_string(), serde_json::json!(json_parse_ms));
        stats.insert("command_ms".to_string(), serde_json::json!(command_ms));
    }

    let serialize_start = std::time::Instant::now();
    let json =
        serde_json::to_string(&output).expect("failed to serialise output");
    let serialize_ms = serialize_start.elapsed().as_millis();

    eprintln!("turbo-rust: total={}ms stdin={}ms json_parse={}ms command={}ms json_serialize={}ms",
        total_start.elapsed().as_millis(), stdin_ms, json_parse_ms, command_ms, serialize_ms);

    print!("{json}");
}
