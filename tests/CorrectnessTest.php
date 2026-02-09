<?php

declare(strict_types=1);

namespace TurboComposer\Tests;

use PHPUnit\Framework\TestCase;

use function array_diff_key;
use function array_filter;
use function array_keys;
use function copy;
use function count;
use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function fwrite;
use function implode;
use function is_dir;
use function ksort;
use function mkdir;
use function preg_match_all;
use function realpath;
use function rmdir;
use function shell_exec;
use function sprintf;
use function str_contains;
use function str_starts_with;
use function sys_get_temp_dir;
use function uniqid;
use function unlink;

use const ARRAY_FILTER_USE_KEY;
use const STDERR;

class CorrectnessTest extends TestCase
{
    private static string $pluginDir;

    public static function setUpBeforeClass(): void
    {
        self::$pluginDir = realpath(__DIR__ . '/..');
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
            $vanillaClassmap = $this->generateVanillaClassmap($workDir);
            $turboClassmap = $this->generateTurboClassmap($workDir);
            $this->assertClassmapsMatch($vanillaClassmap, $turboClassmap, $fixtureName);
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

    private function generateVanillaClassmap(string $workDir): array
    {
        shell_exec("cd {$workDir} && composer remove chrisnetonline/turbo-composer --no-interaction 2>&1");

        shell_exec("cd {$workDir} && composer dump-autoload --optimize --no-interaction 2>&1");

        return $this->parseClassmapFile($workDir . '/vendor/composer/autoload_classmap.php');
    }

    private function generateTurboClassmap(string $workDir): array
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

        return $this->parseClassmapFile($workDir . '/vendor/composer/autoload_classmap.php');
    }

    private function parseClassmapFile(string $path): array
    {
        $this->assertFileExists($path, "Classmap file not found: {$path}");

        $content = file_get_contents($path);
        $classmap = [];

        $pattern = "/^\s*'([^']+)'\s*=>\s*\\$\w+\s*\.\s*'([^']+)'/m";
        preg_match_all($pattern, $content, $matches, PREG_SET_ORDER);

        foreach ($matches as $match) {
            $classmap[$match[1]] = $match[2];
        }

        return $classmap;
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
