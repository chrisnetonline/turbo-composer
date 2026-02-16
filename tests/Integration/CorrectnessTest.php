<?php

declare(strict_types=1);

namespace TurboComposer\Tests\Integration;

use PHPUnit\Framework\TestCase;

use function array_diff;
use function array_diff_key;
use function array_filter;
use function array_keys;
use function array_values;
use function copy;
use function count;
use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function fwrite;
use function implode;
use function is_dir;
use function json_decode;
use function ksort;
use function mkdir;
use function preg_match_all;
use function realpath;
use function rmdir;
use function shell_exec;
use function sort;
use function sprintf;
use function str_contains;
use function str_starts_with;
use function sys_get_temp_dir;
use function tempnam;
use function uniqid;
use function unlink;

use const ARRAY_FILTER_USE_KEY;
use const STDERR;

class CorrectnessTest extends TestCase
{
    private static string $pluginDir;

    public static function setUpBeforeClass(): void
    {
        self::$pluginDir = realpath(__DIR__ . '/../..');
    }

    public function testSymfonyRealFixture(): void
    {
        $this->runCorrectnessCheck('symfony-real');
    }

    public function testLaravelRealFixture(): void
    {
        $this->runCorrectnessCheck('laravel-real');
    }

    public function testMonolithFixture(): void
    {
        $this->runCorrectnessCheck('monolith');
    }

    private function runCorrectnessCheck(string $fixtureName): void
    {
        $fixtureFile = self::$pluginDir . "/benchmarks/fixtures/{$fixtureName}.json";
        if (!file_exists($fixtureFile)) {
            $this->markTestSkipped("Fixture not found: {$fixtureFile}");
        }

        $workDir = $this->createWorkspace($fixtureFile);

        try {
            $this->composerInstall($workDir);

            // 1. Generate vanilla Composer output as baseline
            $vanilla = $this->generateVanillaAutoload($workDir);

            // 2. Cold turbo run (parent::dump() runs alongside Rust)
            $turbo = $this->generateTurboAutoload($workDir);

            $this->assertClassmapsMatch($vanilla['classmap'], $turbo['classmap'], $fixtureName);
            $this->assertAutoloadFilesMatch(
                $vanilla['psr4'],
                $turbo['psr4'],
                $vanilla['namespaces'],
                $turbo['namespaces'],
                $fixtureName,
                'cold',
            );

            // Fixture-aware class expectations: symfony-real maps App\ → src/, so
            // the app/ dummy classes aren't autoloaded; use Src\ classes instead.
            $expectedClasses = match ($fixtureName) {
                'symfony-real' => ['Src\\TestClass1', 'Src\\TestClass30'],
                default => ['App\\TestClass1', 'App\\TestClass50'],
            };
            $this->assertAutoloaderWorks($workDir, $fixtureName, 'cold', $expectedClasses);

            // 3. Warm turbo run (parent::dump() should be skipped since
            //    ClassLoader.php and installed.php exist from the cold run)
            $warmOutput = shell_exec(
                "cd {$workDir} && composer dump-autoload --optimize --no-interaction 2>&1",
            );
            $this->assertNotNull($warmOutput, 'turbo warm dump-autoload failed');
            $this->assertTrue(
                str_contains($warmOutput, 'turbo-composer') || str_contains($warmOutput, 'Rust'),
                "turbo-composer didn't activate on warm run. Output:\n{$warmOutput}",
            );

            $warmTurbo = $this->collectAutoloadData($workDir);

            $this->assertClassmapsMatch($vanilla['classmap'], $warmTurbo['classmap'], $fixtureName);
            $this->assertAutoloadFilesMatch(
                $vanilla['psr4'],
                $warmTurbo['psr4'],
                $vanilla['namespaces'],
                $warmTurbo['namespaces'],
                $fixtureName,
                'warm',
            );
            $this->assertAutoloaderWorks($workDir, $fixtureName, 'warm', $expectedClasses);
        } finally {
            $this->removeDirectory($workDir);
        }
    }

    private function createWorkspace(string $fixtureFile): string
    {
        $dir = sys_get_temp_dir() . '/turbo-composer-test-' . uniqid();
        mkdir($dir, 0o755, true);

        copy($fixtureFile, $dir . '/composer.json');

        foreach (['src', 'app', 'modules', 'database/factories', 'database/seeders', 'database/migrations'] as $sub) {
            mkdir($dir . '/' . $sub, 0o755, true);
        }

        $this->generateDummyClasses($dir . '/src', 'Src', 30);
        $this->generateDummyClasses($dir . '/app', 'App', 50);

        return $dir;
    }

    private function generateDummyClasses(string $dir, string $ns, int $count): void
    {
        for ($i = 1; $i <= $count; $i++) {
            $content = <<<PHP
                <?php

                namespace {$ns};

                class TestClass{$i}
                {
                    public function handle(): void {}
                }
                PHP;

            file_put_contents("{$dir}/TestClass{$i}.php", $content);
        }

        file_put_contents("{$dir}/config.php", "<?php\nreturn ['key' => 'value'];\n");
        file_put_contents("{$dir}/helpers.php", "<?php\nfunction helper_func() {}\n");
    }

    private function composerInstall(string $workDir): void
    {
        $output = shell_exec("cd {$workDir} && composer install --no-interaction --no-autoloader 2>&1");
        $this->assertNotNull($output, 'composer install failed');
        $this->assertStringNotContainsString(
            'Your requirements could not be resolved',
            $output,
            "Dependency resolution failed:\n{$output}",
        );
    }

    /**
     * @return array{classmap: array<string, string>, psr4: list<string>, namespaces: list<string>}
     */
    private function generateVanillaAutoload(string $workDir): array
    {
        shell_exec("cd {$workDir} && composer remove chrisnetonline/turbo-composer --no-interaction 2>&1");

        shell_exec("cd {$workDir} && composer dump-autoload --optimize --no-interaction 2>&1");

        return $this->collectAutoloadData($workDir);
    }

    /**
     * Install turbo-composer and run dump-autoload --optimize (cold run).
     *
     * @return array{classmap: array<string, string>, psr4: list<string>, namespaces: list<string>}
     */
    private function generateTurboAutoload(string $workDir): array
    {
        shell_exec(sprintf('cd %s && composer config repositories.turbo path %s 2>&1', $workDir, self::$pluginDir));
        shell_exec(
            "cd {$workDir} && composer config --no-plugins allow-plugins.chrisnetonline/turbo-composer true 2>&1",
        );

        $output = shell_exec(
            "cd {$workDir} && composer require chrisnetonline/turbo-composer:@dev --no-interaction 2>&1",
        );
        $this->assertNotNull($output);

        $output = shell_exec("cd {$workDir} && composer dump-autoload --optimize --no-interaction 2>&1");
        $this->assertNotNull($output, 'turbo dump-autoload failed');

        $this->assertTrue(
            str_contains($output, 'turbo-composer') || str_contains($output, 'Rust'),
            "turbo-composer didn't activate. Output:\n{$output}",
        );

        return $this->collectAutoloadData($workDir);
    }

    /**
     * Parse classmap, PSR-4 keys, and namespace keys from the generated autoload files.
     *
     * @return array{classmap: array<string, string>, psr4: list<string>, namespaces: list<string>}
     */
    private function collectAutoloadData(string $workDir): array
    {
        $composerDir = $workDir . '/vendor/composer';

        $classmapPath = $composerDir . '/autoload_classmap.php';
        $this->assertFileExists($classmapPath, "Classmap file not found: {$classmapPath}");

        $classmap = [];
        preg_match_all(
            "/^\s*'([^']+)'\s*=>\s*\\$\w+\s*\.\s*'([^']+)'/m",
            file_get_contents($classmapPath),
            $matches,
            PREG_SET_ORDER,
        );
        foreach ($matches as $match) {
            $classmap[$match[1]] = $match[2];
        }

        $extractKeys = static function (string $path): array {
            if (!file_exists($path)) {
                return [];
            }
            preg_match_all("/^\s*'([^']+)'\s*=>/m", file_get_contents($path), $m);
            $keys = $m[1];
            sort($keys);

            return $keys;
        };

        return [
            'classmap' => $classmap,
            'psr4' => $extractKeys($composerDir . '/autoload_psr4.php'),
            'namespaces' => $extractKeys($composerDir . '/autoload_namespaces.php'),
        ];
    }

    private function assertClassmapsMatch(array $vanilla, array $turbo, string $fixtureName): void
    {
        // Filter out classes that naturally differ between runs because the
        // vanilla run has turbo-composer removed while the turbo run has it
        // installed.  Composer\InstalledVersions is regenerated per-run and
        // may differ when the package set changes.
        // Also filter non-ASCII "class names" — Composer's PHP tokeniser
        // sometimes picks up BOM/encoding artefacts as identifiers.
        $filterPredicate = static fn(string $class): bool => (
            !str_starts_with($class, 'TurboComposer\\')
            && $class !== 'Composer\\InstalledVersions'
            && $class !== 'Composer\\\\InstalledVersions'
            && preg_match('/^[A-Za-z0-9_\\\\]+$/', $class) === 1
        );

        $vanilla = array_filter($vanilla, $filterPredicate, ARRAY_FILTER_USE_KEY);
        $turbo = array_filter($turbo, $filterPredicate, ARRAY_FILTER_USE_KEY);

        ksort($vanilla);
        ksort($turbo);

        // Turbo must contain ALL vanilla classes (no missing classes allowed).
        // Extra classes are acceptable — Rust's scanner may find additional
        // secondary/internal classes that Composer's PHP tokeniser skips.
        $missingInTurbo = array_diff_key($vanilla, $turbo);

        if ($missingInTurbo !== []) {
            $missing = implode("\n  ", array_keys($missingInTurbo));
            $this->fail(
                "[{$fixtureName}] Classes in vanilla but missing from turbo "
                . '('
                . count($missingInTurbo)
                . "):\n  {$missing}",
            );
        }

        // Verify paths match for all shared classes.
        $pathMismatches = [];
        foreach ($vanilla as $class => $vanillaPath) {
            $turboPath = $turbo[$class] ?? null;
            if ($vanillaPath !== $turboPath) {
                $pathMismatches[$class] = ['vanilla' => $vanillaPath, 'turbo' => $turboPath];
            }
        }

        if ($pathMismatches !== []) {
            $details = '';
            $shown = 0;
            foreach ($pathMismatches as $class => $paths) {
                $details .= sprintf(
                    "\n  %s:\n    vanilla: %s\n    turbo:   %s",
                    $class,
                    $paths['vanilla'],
                    $paths['turbo'] ?? '(null)',
                );
                if (++$shown >= 20) {
                    $remaining = count($pathMismatches) - $shown;
                    $details .= "\n  … and {$remaining} more";
                    break;
                }
            }

            $this->fail(sprintf('[%s] %d path mismatches:%s', $fixtureName, count($pathMismatches), $details));
        }

        $extraInTurbo = array_diff_key($turbo, $vanilla);
        $this->addToAssertionCount(1);
        if (count($extraInTurbo) > 0) {
            fwrite(STDERR, sprintf(
                "\n  [%s] Note: turbo has %d extra classes (vanilla=%d, turbo=%d)\n",
                $fixtureName,
                count($extraInTurbo),
                count($vanilla),
                count($turbo),
            ));
        }
    }

    /**
     * Compare PSR-4 and PSR-0 namespace registrations between vanilla and turbo.
     *
     * @param list<string> $vanillaPsr4
     * @param list<string> $turboPsr4
     * @param list<string> $vanillaNamespaces
     * @param list<string> $turboNamespaces
     */
    private function assertAutoloadFilesMatch(
        array $vanillaPsr4,
        array $turboPsr4,
        array $vanillaNamespaces,
        array $turboNamespaces,
        string $fixtureName,
        string $label,
    ): void {
        // Filter out TurboComposer's own namespace — it's only present in the turbo run
        $filterTurbo = static fn(string $ns): bool => !str_starts_with($ns, 'TurboComposer\\');

        $turboPsr4 = array_values(array_filter($turboPsr4, $filterTurbo));
        $turboNamespaces = array_values(array_filter($turboNamespaces, $filterTurbo));

        $missingPsr4 = array_diff($vanillaPsr4, $turboPsr4);
        if ($missingPsr4 !== []) {
            $this->fail(sprintf(
                "[%s %s] PSR-4 namespaces in vanilla but missing from turbo:\n  %s",
                $fixtureName,
                $label,
                implode("\n  ", $missingPsr4),
            ));
        }

        $missingNamespaces = array_diff($vanillaNamespaces, $turboNamespaces);
        if ($missingNamespaces !== []) {
            $this->fail(sprintf(
                "[%s %s] PSR-0 namespaces in vanilla but missing from turbo:\n  %s",
                $fixtureName,
                $label,
                implode("\n  ", $missingNamespaces),
            ));
        }

        $this->addToAssertionCount(1);
    }

    /**
     * Run a PHP subprocess that requires the generated autoloader and verifies
     * that it can actually resolve classes. This catches issues in autoload.php,
     * autoload_real.php, autoload_static.php, and the classmap files.
     *
     * @param list<string> $expectedClasses FQCN of classes that must be resolvable
     */
    private function assertAutoloaderWorks(
        string $workDir,
        string $fixtureName,
        string $label,
        array $expectedClasses,
    ): void {
        $classChecks = '';
        foreach ($expectedClasses as $i => $fqcn) {
            $classChecks .= "    'class_{$i}' => class_exists('{$fqcn}', true),\n";
        }

        $script = <<<PHP
            <?php
            \$autoloadPath = \$argv[1] . '/vendor/autoload.php';
            if (!file_exists(\$autoloadPath)) {
                echo json_encode(['error' => 'autoload.php not found']);
                exit(1);
            }

            \$loader = require \$autoloadPath;

            \$results = [
                'loader_valid' => (\$loader instanceof \\Composer\\Autoload\\ClassLoader),
            {$classChecks}];

            echo json_encode(\$results);
            PHP;

        $scriptPath = tempnam(sys_get_temp_dir(), 'turbo-autoload-test-');
        file_put_contents($scriptPath, $script);

        try {
            $output = shell_exec(sprintf('php %s %s 2>&1', $scriptPath, $workDir));
            $this->assertNotNull($output, "[{$fixtureName} {$label}] Autoload subprocess returned null");

            $results = json_decode($output, true);
            $this->assertIsArray($results, "[{$fixtureName} {$label}] Autoload output not valid JSON: {$output}");

            $this->assertTrue(
                $results['loader_valid'] ?? false,
                "[{$fixtureName} {$label}] ClassLoader not returned from autoload.php",
            );

            foreach ($expectedClasses as $i => $fqcn) {
                $this->assertTrue(
                    $results["class_{$i}"] ?? false,
                    "[{$fixtureName} {$label}] {$fqcn} not resolvable via autoloader",
                );
            }
        } finally {
            unlink($scriptPath);
        }
    }

    private function removeDirectory(string $dir): void
    {
        if (!is_dir($dir)) {
            return;
        }

        $items = new \RecursiveIteratorIterator(
            new \RecursiveDirectoryIterator($dir, \FilesystemIterator::SKIP_DOTS),
            \RecursiveIteratorIterator::CHILD_FIRST,
        );

        foreach ($items as $item) {
            $path = $item->getPathname();
            if ($item->isDir() && !is_link($path)) {
                rmdir($path);
                continue;
            }
            unlink($path);
        }

        rmdir($dir);
    }
}
