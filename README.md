<p align="center">
  <img src="logo.png" alt="Turbo Composer" width="300">
</p>

# turbo-composer

[![Release](https://github.com/chrisnetonline/turbo-composer/actions/workflows/release.yml/badge.svg)](https://github.com/chrisnetonline/turbo-composer/actions/workflows/release.yml)

Rust-powered Composer acceleration: parallel extraction, fast classmap generation, integrity verification, and vendor validation.

## Features

- **Fast classmap generation** — byte-scanning PHP tokenizer with parallel directory walking, up to 7x faster than Composer's built-in classmap generator
- **Incremental caching** — caches symbols by file mtime with directory-level cache; warm runs skip entire directory walks and vendor stat calls
- **Staged file writes** — Rust writes autoload files directly to disk with atomic rename, eliminating JSON serialization overhead for large classmaps
- **Smart parent::dump() skip** — when infrastructure files already exist from a prior install, the Composer PHP-side dump is skipped entirely on warm runs
- **Batched operations** — clean, verify, and extract operations are combined into a single Rust process invocation, reducing process spawn overhead
- **Parallel package extraction** — extracts zip/tar archives using Rust + rayon for parallel I/O
- **Parallel integrity verification** — SHA256/SHA1 hash verification of package archives, ~7x faster than PHP's `hash_file()`
- **Fast vendor state validation** — checks all packages are present in vendor/ in parallel, up to 13x faster than PHP
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

1. Resolves the autoloader suffix and builds the autoload payload in PHP
2. Starts the Rust engine as a background subprocess with a staging suffix (`.turbo`)
3. Rust walks all directories in parallel (two-phase: collect paths, then rayon parallel read+parse), extracts class/interface/trait/enum symbols using a single-pass byte scanner, and writes all 7 autoload files directly to disk as staged files
4. If infrastructure files (`ClassLoader.php`, `installed.php`) already exist, Composer's `parent::dump()` is skipped entirely; otherwise it runs in parallel with Rust
5. Once both complete, the staged `.turbo` files are atomically renamed to their final names

Rust generates all autoload files directly: `autoload.php`, `autoload_real.php`, `autoload_classmap.php`, `autoload_psr4.php`, `autoload_namespaces.php`, `autoload_files.php`, and `autoload_static.php`. An incremental mtime cache skips re-reading unchanged files, and vendor files skip stat calls entirely on warm runs.

During `composer install`/`update`, clean, verify, and extract operations are batched into a single Rust process invocation to minimize process spawn overhead.

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
| symfony-real | 4,334 | 4,917ms | 696ms (**7.1x**) | 741ms (**6.6x**) |
| laravel-real | 5,594 | 2,795ms | 845ms (**3.3x**) | 804ms (**3.5x**) |
| monolith | 8,874 | 3,886ms | 1,055ms (**3.7x**) | 1,062ms (**3.7x**) |

Rust writes all 7 autoload files directly to disk using a staging + atomic rename approach, eliminating JSON serialization overhead. On warm runs, `parent::dump()` is skipped when infrastructure files already exist, and the incremental mtime cache skips entire directory walks and vendor stat calls.

### Integrity Verification (SHA256)

| Fixture | Files | Rust | PHP | Speedup |
|---|---|---|---|---|
| symfony-real | 200 | 23ms | 169ms | **7.3x** |
| laravel-real | 200 | 113ms | 201ms | **1.8x** |
| monolith | 200 | 24ms | 158ms | **6.6x** |

Parallel SHA256 hashing using Rust's `sha2` crate + rayon, vs PHP's sequential `hash_file()`.

### Vendor State Validation

| Fixture | Packages | Rust | PHP | Speedup |
|---|---|---|---|---|
| symfony-real | 70 | 124ms | 214ms | **1.7x** |
| laravel-real | 76 | 14ms | 136ms | **9.7x** |
| monolith | 115 | 9ms | 118ms | **13.1x** |

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
