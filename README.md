<p align="center">
  <img src="logo.png" alt="Turbo Composer" width="300">
</p>

# turbo-composer

[![Release](https://github.com/chrisnetonline/turbo-composer/actions/workflows/release.yml/badge.svg)](https://github.com/chrisnetonline/turbo-composer/actions/workflows/release.yml)

Rust-powered Composer acceleration: parallel extraction, fast classmap generation, integrity verification, and vendor validation.

## Features

- **Fast classmap generation** — byte-scanning PHP tokenizer with parallel directory walking, up to 3.9x faster than Composer's built-in classmap generator
- **Incremental caching** — caches symbols by file mtime with directory-level cache; warm runs skip entire directory walks and vendor stat calls
- **Parallel package extraction** — extracts zip/tar archives using Rust + rayon for parallel I/O
- **Parallel integrity verification** — SHA256/SHA1 hash verification of package archives, ~7x faster than PHP's `hash_file()`
- **Fast vendor state validation** — checks all packages are present in vendor/ in parallel, up to 13.5x faster than PHP
- **Parallel vendor cleanup** — concurrent removal of package directories during updates/uninstalls
- **Drop-in Composer plugin** — integrates transparently with `composer install` and `composer dump-autoload -o`
- **Automatic binary management** — downloads platform-specific binaries on first use

## Installation

```bash
composer require chrisnetonline/turbo-composer
```

Once installed, the plugin activates automatically. Run Composer as usual:

```bash
composer install
composer dump-autoload --optimize
```

## How it works

turbo-composer replaces Composer's autoload generator with a Rust-accelerated version. When you run `composer dump-autoload --optimize`, it:

1. Resolves the autoloader suffix and starts the Rust engine as a background subprocess
2. While Rust works, Composer generates the base (non-optimized) autoload files in parallel
3. Rust walks all directories in parallel (two-phase: collect paths, then rayon parallel read+parse), and extracts class/interface/trait/enum symbols using a single-pass byte scanner. An incremental mtime cache skips re-reading unchanged files, and vendor files skip stat calls entirely on warm runs.
4. Rust generates all 5 autoload data files (`autoload_classmap.php`, `autoload_psr4.php`, `autoload_namespaces.php`, `autoload_files.php`, `autoload_static.php`) directly

The Rust engine communicates with PHP by spawning as a subprocess, receiving JSON over stdin and returning results via stdout.

## Configuration

The plugin works out of the box with zero configuration. The binary version is automatically matched to the installed plugin version.

For advanced use cases (e.g., private mirrors), you can override the download URL in your project's `composer.json`:

```json
{
    "extra": {
        "turbo-composer": {
            "base-url": "https://my-internal-mirror.example.com/turbo-composer/releases/download"
        }
    }
}
```

| Setting | Default | Description |
|---|---|---|
| `base-url` | GitHub releases URL | Override base URL for binary downloads |

## Platform support

| Platform | Architecture | Binary |
|---|---|---|
| Linux | x86_64 | Yes |
| Linux | aarch64 | Yes |
| macOS | x86_64 | Yes |
| macOS | aarch64 (Apple Silicon) | Yes |
| Windows | x86_64 | Yes |

## Development

### Prerequisites

- PHP 8.1+
- Composer 2.6+
- Rust toolchain (`cargo`, `rustc`, `clippy`) — install via [rustup](https://rustup.rs) or `brew install rust`

### Building from source

```bash
cd rust
cargo build --release
cp target/release/turbo-composer ../bin/turbo-composer
```

### PHP checks

```bash
composer install

# Formatting (mago)
vendor/bin/mago fmt            # Format PHP files
vendor/bin/mago fmt --check    # Check formatting without modifying

# Linting (mago)
vendor/bin/mago lint           # Run static analysis
vendor/bin/mago lint --fix     # Auto-fix lint issues

# Unit tests (no Rust binary needed)
vendor/bin/phpunit --testsuite Unit

# Correctness tests (requires Rust binary in bin/)
vendor/bin/phpunit --testsuite Correctness

# All PHP tests
vendor/bin/phpunit
```

The **unit tests** cover `BinaryInstaller` and `RustBridge` using fake shell-script binaries — no Rust build required. The **correctness tests** generate classmaps with both vanilla Composer and turbo-composer across multiple fixtures (small, medium, large, laravel-app, symfony-app) and assert they match exactly.

### Rust checks

```bash
cd rust

cargo check              # Type-check without building
cargo test               # Run unit + integration tests
cargo clippy -- -D warnings   # Lint with clippy (warnings as errors)
```

### Benchmarks

```bash
chmod +x benchmarks/run.sh
./benchmarks/run.sh         # 3 iterations per fixture (default)
./benchmarks/run.sh 5       # 5 iterations per fixture
```

The benchmark script builds the binary if needed, creates temporary projects with varying sizes, and compares vanilla Composer vs turbo-composer (cold cache vs warm cache).

## Performance

### Classmap Generation (`dump-autoload --optimize`)

| Fixture | PHP Files | Vanilla | Turbo (cold) | Turbo (warm) |
|---|---|---|---|---|
| symfony-real | 4,334 | 2,728ms | 700ms (**3.9x**) | 798ms (**3.4x**) |
| laravel-real | 5,594 | 2,607ms | 817ms (**3.2x**) | 836ms (**3.1x**) |
| monolith | 8,874 | 3,903ms | 1,786ms (**2.2x**) | 1,258ms (**3.1x**) |

Rust generates all 5 autoload data files (classmap, psr4, namespaces, files, static) in a single call. The Rust classmap engine runs in parallel with Composer's base dump via `proc_open`, so the Rust work is effectively free when it finishes before PHP. An incremental mtime cache stores parsed symbols per file, and a directory-level mtime cache skips entire directory walks and vendor stat calls on warm runs.

### Integrity Verification (SHA256)

| Fixture | Files | Rust | PHP | Speedup |
|---|---|---|---|---|
| symfony-real | 200 | 25ms | 166ms | **6.6x** |
| laravel-real | 200 | 24ms | 169ms | **7.0x** |
| monolith | 200 | 24ms | 166ms | **6.6x** |

Parallel SHA256 hashing using Rust's `sha2` crate + rayon, vs PHP's sequential `hash_file()`.

### Vendor State Validation

| Fixture | Packages | Rust | PHP | Speedup |
|---|---|---|---|---|
| symfony-real | 70 | 127ms | 256ms | **1.9x** |
| laravel-real | 76 | 9ms | 125ms | **13.3x** |
| monolith | 115 | 9ms | 122ms | **13.5x** |

Parallel check that all packages from `composer.lock` are present and non-empty in vendor/. Useful for CI warm-cache validation.

## Architecture

```
src/
  TurboInstallerPlugin.php   # Composer plugin entry point
  TurboAutoloadGenerator.php # Extends Composer's AutoloadGenerator
  RustBridge.php             # Spawns Rust binary, communicates via JSON
  BinaryInstaller.php        # Downloads/installs platform binary

rust/src/
  lib.rs                     # Library entry (exports modules)
  main.rs                    # CLI binary entry
  classmap/                  # Classmap generation module
    mod.rs                   #   Public API + orchestrator
    parser.rs                #   PHP symbol extraction (byte scanner)
    walker.rs                #   Parallel directory walking + file parsing
    codegen.rs               #   PHP autoload file generation
    cache.rs                 #   Incremental mtime caching
  extract.rs                 # Parallel package extraction
  clean.rs                   # Parallel vendor directory cleanup
  verify.rs                  # Parallel SHA256/SHA1 integrity verification
  vendor_state.rs            # Fast vendor directory validation
```

## License

MIT
