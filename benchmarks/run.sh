#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="$SCRIPT_DIR/results"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"
ITERATIONS="${1:-3}"

mkdir -p "$RESULTS_DIR"

echo "=== turbo-composer benchmarks ==="
echo "Iterations per fixture: $ITERATIONS"
echo ""

# Detect platform for binary name
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$OS" in
    darwin) PLATFORM="darwin" ;;
    linux)  PLATFORM="linux" ;;
    *)      echo "Unsupported OS: $OS"; exit 1 ;;
esac
case "$ARCH" in
    x86_64|amd64)    ARCH="x86_64" ;;
    aarch64|arm64)   ARCH="aarch64" ;;
    *)               echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

BINARY="$PROJECT_DIR/bin/turbo-composer-${PLATFORM}-${ARCH}"
if [ ! -x "$BINARY" ]; then
    echo "Building Rust binary..."
    (cd "$PROJECT_DIR/rust" && cargo build --release 2>/dev/null)
    mkdir -p "$PROJECT_DIR/bin"
    cp "$PROJECT_DIR/rust/target/release/turbo-composer" "$BINARY"
    chmod 755 "$BINARY"
    echo "Binary built: $BINARY"
fi
echo ""

median() {
    local -a arr=("$@")
    IFS=$'\n' sorted=($(sort -n <<<"${arr[*]}")); unset IFS
    local len=${#sorted[@]}
    echo "${sorted[$((len / 2))]}"
}

FIXTURES=("symfony-real" "laravel-real" "monolith")

for fixture_name in "${FIXTURES[@]}"; do
    fixture="$FIXTURES_DIR/${fixture_name}.json"
    if [ ! -f "$fixture" ]; then
        echo "⚠ Fixture not found: $fixture — skipping"
        continue
    fi
    name="$fixture_name"
    echo "━━━ Fixture: $name ━━━"

    workdir="$(mktemp -d)"
    cp "$fixture" "$workdir/composer.json"

    mkdir -p "$workdir"/{src,app,modules,database/{factories,seeders,migrations}}
    mkdir -p "$workdir"/src/{Domain,Infrastructure}

    case "$name" in
        monolith|large*)
            app_count=200; src_count=150; module_count=100 ;;
        *laravel*|medium*)
            app_count=100; src_count=80; module_count=50 ;;
        *)
            app_count=50; src_count=30; module_count=0 ;;
    esac

    for i in $(seq 1 "$app_count"); do
        cat > "$workdir/app/AppClass${i}.php" <<PHP
<?php
namespace App;
class AppClass${i} { public function handle(): void {} }
PHP
    done

    for i in $(seq 1 "$src_count"); do
        cat > "$workdir/src/SrcClass${i}.php" <<PHP
<?php
namespace Src;
class SrcClass${i} { public function execute(): mixed { return null; } }
PHP
    done

    for i in $(seq 1 "$module_count"); do
        cat > "$workdir/modules/Module${i}.php" <<PHP
<?php
namespace Modules;
class Module${i} { public function boot(): void {} }
PHP
    done

    for i in $(seq 1 20); do
        cat > "$workdir/database/migrations/2024_01_${i}_create_table.php" <<PHP
<?php
use Illuminate\Database\Migrations\Migration;
class CreateTable${i} extends Migration { public function up(): void {} public function down(): void {} }
PHP
    done

    cd "$workdir"
    echo -n "  Installing dependencies... "
    install_output=$(composer install --no-interaction --no-autoloader 2>&1) || true
    echo "done"

    vendor_php_count=0
    if [ -d "$workdir/vendor" ]; then
        vendor_php_count=$(find "$workdir/vendor" -name '*.php' -type f 2>/dev/null | wc -l)
    fi
    app_php_count=$(find "$workdir" -maxdepth 3 -not -path '*/vendor/*' -name '*.php' -type f 2>/dev/null | wc -l)
    echo "  PHP files: ${app_php_count} app + ${vendor_php_count} vendor = $((app_php_count + vendor_php_count)) total"

    vanilla_times=()
    for iter in $(seq 1 "$ITERATIONS"); do
        rm -f "$workdir/vendor/composer/autoload_classmap.php" 2>/dev/null || true

        start=$(date +%s%N)
        output=$(composer dump-autoload --optimize --no-interaction 2>&1) || true
        end=$(date +%s%N)
        ms=$(( (end - start) / 1000000 ))
        vanilla_times+=("$ms")
    done

    vanilla_classes=$(echo "$output" | sed -n 's/.*containing \([0-9]*\).*/\1/p' | tail -1)
    vanilla_classes="${vanilla_classes:-?}"
    vanilla_median=$(median "${vanilla_times[@]}")
    echo "  Vanilla:  ${vanilla_median}ms (median of ${ITERATIONS} runs, ${vanilla_classes} classes)"

    composer config repositories.turbo path "$PROJECT_DIR" 2>/dev/null || true
    composer config --no-plugins allow-plugins.chrisnetonline/turbo-composer true 2>/dev/null || true
    require_output=$(composer require chrisnetonline/turbo-composer:@dev --no-interaction 2>&1) || true

    turbo_dir="$workdir/vendor/turbo-composer"
    if [ -x "$BINARY" ] && [ -d "$turbo_dir" ]; then
        cp "$BINARY" "$turbo_dir/turbo-composer" 2>/dev/null || true
        chmod 755 "$turbo_dir/turbo-composer" 2>/dev/null || true
    fi

    turbo_active=false
    if echo "$require_output" | grep -q "Rust acceleration active"; then
        turbo_active=true
    fi

    COMPOSER_BIN="$(which composer)"

    rm -f "$workdir/vendor/composer/.turbo-cache" 2>/dev/null || true
    turbo_cold_times=()
    turbo_output=""
    for iter in $(seq 1 "$ITERATIONS"); do
        rm -f "$workdir/vendor/composer/autoload_classmap.php" 2>/dev/null || true
        rm -f "$workdir/vendor/composer/.turbo-cache" 2>/dev/null || true

        start=$(date +%s%N)
        output=$(php "$COMPOSER_BIN" dump-autoload --optimize --no-interaction 2>&1) || true
        end=$(date +%s%N)
        ms=$(( (end - start) / 1000000 ))
        turbo_cold_times+=("$ms")
        turbo_output="$output"
    done

    if echo "$turbo_output" | grep -q "Rust-accelerated\|turbo-composer.*classes mapped"; then
        turbo_active=true
    fi

    turbo_cold_classes=$(echo "$turbo_output" | sed -n 's/.*✓ \([0-9]*\).*/\1/p' | tail -1)
    turbo_cold_classes="${turbo_cold_classes:-?}"
    turbo_cold_median=$(median "${turbo_cold_times[@]}")
    echo -n "  Turbo (cold):  ${turbo_cold_median}ms (median of ${ITERATIONS} runs, ${turbo_cold_classes} classes)"

    if [ "$turbo_active" = false ]; then
        echo "  ⚠ WARNING: turbo-composer did NOT activate"
    fi

    cold_speedup="N/A"
    if [ "$vanilla_median" -gt 0 ] && [ "$turbo_cold_median" -gt 0 ]; then
        cold_speedup=$(echo "scale=2; $vanilla_median / $turbo_cold_median" | bc 2>/dev/null || echo "N/A")
    fi
    echo " — ${cold_speedup}x"

    php "$COMPOSER_BIN" dump-autoload --optimize --no-interaction >/dev/null 2>&1 || true

    turbo_warm_times=()
    for iter in $(seq 1 "$ITERATIONS"); do
        rm -f "$workdir/vendor/composer/autoload_classmap.php" 2>/dev/null || true
        start=$(date +%s%N)
        output=$(php "$COMPOSER_BIN" dump-autoload --optimize --no-interaction 2>&1) || true
        end=$(date +%s%N)
        ms=$(( (end - start) / 1000000 ))
        turbo_warm_times+=("$ms")
        turbo_warm_output="$output"
    done

    turbo_warm_classes=$(echo "$turbo_warm_output" | sed -n 's/.*✓ \([0-9]*\).*/\1/p' | tail -1)
    turbo_warm_classes="${turbo_warm_classes:-?}"
    turbo_warm_median=$(median "${turbo_warm_times[@]}")

    walk_skipped=""
    if echo "$turbo_warm_output" | grep -q "walk skipped"; then
        walk_skipped=" [walk skipped]"
    fi

    warm_speedup="N/A"
    if [ "$vanilla_median" -gt 0 ] && [ "$turbo_warm_median" -gt 0 ]; then
        warm_speedup=$(echo "scale=2; $vanilla_median / $turbo_warm_median" | bc 2>/dev/null || echo "N/A")
    fi
    echo "  Turbo (warm):  ${turbo_warm_median}ms (median of ${ITERATIONS} runs, ${turbo_warm_classes} classes)${walk_skipped} — ${warm_speedup}x"

    # --- Vendor State Validation Benchmark ---
    vendor_check_ms="N/A"
    php_vendor_check_ms="N/A"
    pkg_count=0

    if [ -f "$workdir/composer.lock" ]; then
        # Build the vendor-check payload from composer.lock
        vendor_check_json=$(php -r '
            $lock = json_decode(file_get_contents("'"$workdir"'/composer.lock"), true);
            $pkgs = [];
            foreach ($lock["packages"] ?? [] as $p) {
                $pkgs[] = ["name" => $p["name"], "install_path" => realpath("'"$workdir"'/vendor/" . $p["name"]) ?: "'"$workdir"'/vendor/" . $p["name"]];
            }
            echo json_encode(["command" => "vendor-check", "check_packages" => $pkgs]);
        ')
        pkg_count=$(echo "$vendor_check_json" | php -r 'echo count(json_decode(file_get_contents("php://stdin"), true)["check_packages"] ?? []);')

        if [ "$pkg_count" -gt 0 ]; then
            # Rust vendor-check
            start=$(date +%s%N)
            echo "$vendor_check_json" | "$BINARY" 2>/dev/null >/dev/null
            end=$(date +%s%N)
            vendor_check_ms=$(( (end - start) / 1000000 ))

            # PHP equivalent
            start=$(date +%s%N)
            php -r '
                $lock = json_decode(file_get_contents("'"$workdir"'/composer.lock"), true);
                $present = 0; $missing = 0;
                foreach ($lock["packages"] ?? [] as $p) {
                    $path = "'"$workdir"'/vendor/" . $p["name"];
                    if (is_dir($path)) { $present++; } else { $missing++; }
                }
            '
            end=$(date +%s%N)
            php_vendor_check_ms=$(( (end - start) / 1000000 ))

            vc_speedup="N/A"
            if [ "$vendor_check_ms" -gt 0 ] && [ "$php_vendor_check_ms" -gt 0 ]; then
                vc_speedup=$(echo "scale=1; $php_vendor_check_ms / $vendor_check_ms" | bc 2>/dev/null || echo "N/A")
            fi
            echo "  Vendor check: ${vendor_check_ms}ms Rust vs ${php_vendor_check_ms}ms PHP (${pkg_count} packages) — ${vc_speedup}x"
        fi
    fi

    # --- Package Integrity Verification Benchmark ---
    verify_ms="N/A"
    php_verify_ms="N/A"
    verify_count=0

    if [ -d "$workdir/vendor" ]; then
        # Collect first 50 PHP files from vendor for a representative verify benchmark
        verify_json=$(php -r '
            $dir = "'"$workdir"'/vendor";
            $iter = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($dir, FilesystemIterator::SKIP_DOTS));
            $targets = [];
            $count = 0;
            foreach ($iter as $file) {
                if ($file->getExtension() !== "php") continue;
                $path = $file->getPathname();
                $hash = hash_file("sha256", $path);
                $targets[] = ["path" => $path, "name" => basename($path), "algorithm" => "sha256", "expected_hash" => $hash];
                if (++$count >= 200) break;
            }
            echo json_encode(["command" => "verify", "verify_targets" => $targets]);
        ')
        verify_count=$(echo "$verify_json" | php -r 'echo count(json_decode(file_get_contents("php://stdin"), true)["verify_targets"] ?? []);')

        if [ "$verify_count" -gt 0 ]; then
            # Rust verify (parallel SHA256)
            start=$(date +%s%N)
            echo "$verify_json" | "$BINARY" 2>/dev/null >/dev/null
            end=$(date +%s%N)
            verify_ms=$(( (end - start) / 1000000 ))

            # PHP verify (sequential hash_file)
            start=$(date +%s%N)
            php -r '
                $data = json_decode('"'"''"$verify_json"''"'"', true);
                foreach ($data["verify_targets"] as $t) {
                    $actual = hash_file("sha256", $t["path"]);
                    if ($actual !== $t["expected_hash"]) { echo "MISMATCH\n"; }
                }
            '
            end=$(date +%s%N)
            php_verify_ms=$(( (end - start) / 1000000 ))

            vf_speedup="N/A"
            if [ "$verify_ms" -gt 0 ] && [ "$php_verify_ms" -gt 0 ]; then
                vf_speedup=$(echo "scale=1; $php_verify_ms / $verify_ms" | bc 2>/dev/null || echo "N/A")
            fi
            echo "  Verify (SHA256): ${verify_ms}ms Rust vs ${php_verify_ms}ms PHP (${verify_count} files) — ${vf_speedup}x"
        fi
    fi

    cat > "$RESULTS_DIR/${name}.json" <<JSON
{
    "fixture": "$name",
    "vanilla_ms": $vanilla_median,
    "turbo_cold_ms": $turbo_cold_median,
    "turbo_warm_ms": $turbo_warm_median,
    "vanilla_classes": "$vanilla_classes",
    "turbo_cold_classes": "$turbo_cold_classes",
    "turbo_warm_classes": "$turbo_warm_classes",
    "turbo_active": $turbo_active,
    "iterations": $ITERATIONS,
    "vanilla_all_ms": [$(IFS=,; echo "${vanilla_times[*]}")],
    "turbo_cold_all_ms": [$(IFS=,; echo "${turbo_cold_times[*]}")],
    "turbo_warm_all_ms": [$(IFS=,; echo "${turbo_warm_times[*]}")],
    "php_files_app": $app_php_count,
    "php_files_vendor": $vendor_php_count,
    "vendor_check_rust_ms": "$vendor_check_ms",
    "vendor_check_php_ms": "$php_vendor_check_ms",
    "vendor_check_packages": $pkg_count,
    "verify_rust_ms": "$verify_ms",
    "verify_php_ms": "$php_verify_ms",
    "verify_files": $verify_count
}
JSON

    rm -rf "$workdir"
    echo ""
done

echo "=== Results saved to $RESULTS_DIR ==="
