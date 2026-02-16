<?php

declare(strict_types=1);

namespace TurboComposer;

use Composer\Autoload\AutoloadGenerator;
use Composer\Config;
use Composer\EventDispatcher\EventDispatcher;
use Composer\Installer\InstallationManager;
use Composer\IO\IOInterface;
use Composer\Package\AliasPackage;
use Composer\Package\Locker;
use Composer\Package\RootPackageInterface;
use Composer\Repository\InstalledRepositoryInterface;

use function array_key_exists;
use function array_merge;
use function file_exists;
use function file_get_contents;
use function microtime;
use function preg_match;
use function rename;
use function round;
use function rtrim;
use function str_starts_with;
use function unlink;

class TurboAutoloadGenerator extends AutoloadGenerator
{
    private IOInterface $io;
    private RustBridge $bridge;
    private bool $turboDevMode = false;

    private const STAGING_SUFFIX = '.turbo';

    /** Files that Rust writes to the target dir (vendor/composer) */
    private const STAGED_TARGET_FILES = [
        'autoload_classmap.php',
        'autoload_psr4.php',
        'autoload_namespaces.php',
        'autoload_files.php',
        'autoload_static.php',
        'autoload_real.php',
    ];

    public function __construct(EventDispatcher $eventDispatcher, IOInterface $io, RustBridge $bridge)
    {
        parent::__construct($eventDispatcher, $io);
        $this->io = $io;
        $this->bridge = $bridge;
    }

    public function setDevMode(bool $devMode = true): void
    {
        $this->turboDevMode = $devMode;
        parent::setDevMode($devMode);
    }

    public function dump(
        Config $config,
        InstalledRepositoryInterface $localRepo,
        RootPackageInterface $rootPackage,
        InstallationManager $installationManager,
        string $targetDir,
        bool $scanPsrPackages = false,
        ?string $suffix = null,
        ?Locker $locker = null,
        bool $strictAmbiguous = false,
    ) {
        if (!$scanPsrPackages) {
            $this->io->write(
                '<info>turbo-composer:</info> Non-optimised dump — using default generator.',
                true,
                IOInterface::VERBOSE,
            );
            return parent::dump(
                $config,
                $localRepo,
                $rootPackage,
                $installationManager,
                $targetDir,
                false,
                $suffix,
                $locker,
                $strictAmbiguous,
            );
        }

        $totalStart = microtime(true);
        $this->io->write('<info>turbo-composer:</info> Rust-accelerated classmap generation…');

        $vendorDir = $config->get('vendor-dir');
        $resolvedSuffix = $suffix !== '' ? $suffix : null;
        $resolvedSuffix ??= $config->get('autoloader-suffix');

        if ($resolvedSuffix === null) {
            $autoloadPhp = $vendorDir . '/autoload.php';
            if (file_exists($autoloadPhp)) {
                $content = (string) file_get_contents($autoloadPhp);
                if (preg_match('{ComposerAutoloaderInit([^:\s]+)::}', $content, $match)) {
                    $resolvedSuffix = $match[1];
                }
            }
        }

        $t0 = microtime(true);
        $projectDir = dirname($vendorDir);
        $absTargetDir = str_starts_with($targetDir, '/') ? $targetDir : $vendorDir . '/' . $targetDir;
        $payload = $this->buildPayload($projectDir, $vendorDir, $localRepo, $rootPackage, $installationManager);
        $payload['target_dir'] = $absTargetDir;
        $payload['staging_suffix'] = self::STAGING_SUFFIX;
        $payload['has_platform_check'] = file_exists($absTargetDir . '/platform_check.php');
        $payload['has_files_autoload'] = $payload['autoload']['files'] !== [];

        if ($resolvedSuffix !== null) {
            $payload['suffix'] = $resolvedSuffix;
        }
        $buildPayloadMs = round((microtime(true) - $t0) * 1000);

        // Decide whether we can skip parent::dump entirely.
        // When Rust generates autoload.php + autoload_real.php (requires suffix),
        // and the infrastructure files already exist from a previous dump,
        // there's no need to run parent::dump at all.
        $canSkipParentDump =
            $resolvedSuffix !== null
            && file_exists($absTargetDir . '/ClassLoader.php')
            && file_exists($absTargetDir . '/installed.php');

        // Start Rust in the background — it writes staged files directly to disk
        $collect = $this->bridge->startAsync($payload);

        $parentDumpMs = 0.0;
        $result = null;
        if (!$canSkipParentDump) {
            $t0 = microtime(true);
            $result = parent::dump(
                $config,
                $localRepo,
                $rootPackage,
                $installationManager,
                $targetDir,
                false,
                $suffix,
                $locker,
                $strictAmbiguous,
            );
            $parentDumpMs = round((microtime(true) - $t0) * 1000);
        }

        // Collect the Rust result (Rust writes files with .turbo suffix)
        $t0 = microtime(true);
        $rustResult = $collect !== null ? $collect() : null;
        $rustBridgeMs = round((microtime(true) - $t0) * 1000);

        // If suffix wasn't available before, Rust couldn't generate autoload_real.php
        // or autoload_static.php. Rare edge case (first ever install without a lock file).
        if ($resolvedSuffix === null && $rustResult !== null) {
            $autoloadPhp = $vendorDir . '/autoload.php';
            if (file_exists($autoloadPhp)) {
                $content = (string) file_get_contents($autoloadPhp);
                if (preg_match('{ComposerAutoloaderInit([^:\s]+)::}', $content, $match)) {
                    $payload['suffix'] = $match[1];
                    $rustResult = $this->bridge->run($payload);
                }
            }
        }

        if ($rustResult === null) {
            // Clean up any partially staged files
            $this->cleanStagedFiles($absTargetDir, $vendorDir);

            $this->io->writeError('<warning>turbo-composer:</warning> Rust binary failed — '
            . 're-running with default Composer optimisation…');
            return parent::dump(
                $config,
                $localRepo,
                $rootPackage,
                $installationManager,
                $targetDir,
                true,
                $suffix,
                $locker,
                $strictAmbiguous,
            );
        }

        // Promote staged files — atomic rename overwrites parent::dump's versions
        $this->promoteStagedFiles($absTargetDir, $vendorDir);

        $totalMs = round((microtime(true) - $totalStart) * 1000);
        $count = $rustResult['classmap_count'] ?? 0;
        $stats = $rustResult['stats'] ?? [];
        $walkSkipped = $stats['walk_skipped'] ?? false ? ' (walk skipped)' : '';
        $skippedLabel = $canSkipParentDump ? ' [skipped]' : '';
        $parallel = !$canSkipParentDump && $resolvedSuffix !== null ? ' [parallel]' : '';
        $this->io->write("<info>turbo-composer:</info> ✓ {$count} classes mapped in {$totalMs}ms");
        $this->io->write(
            "<info>turbo-composer:</info>   ├─ parent::dump (base):     {$parentDumpMs}ms{$parallel}{$skippedLabel}",
        );
        $this->io->write("<info>turbo-composer:</info>   ├─ buildPayload (PHP):      {$buildPayloadMs}ms");
        $this->io->write("<info>turbo-composer:</info>   ├─ Rust bridge (collect):   {$rustBridgeMs}ms");
        $this->io->write(
            '<info>turbo-composer:</info>   │  ├─ walk+parse:           '
            . ($stats['walk_ms'] ?? '?')
            . "ms{$walkSkipped}",
        );
        $this->io->write(
            '<info>turbo-composer:</info>   │  └─ generate+write:       ' . ($stats['generate_ms'] ?? '?') . 'ms',
        );

        return $result;
    }

    /**
     * Rename staged files (.turbo suffix) to their final names.
     * This is atomic on POSIX systems (same filesystem).
     */
    private function promoteStagedFiles(string $targetDir, string $vendorDir): void
    {
        foreach (self::STAGED_TARGET_FILES as $file) {
            $staged = $targetDir . '/' . $file . self::STAGING_SUFFIX;
            if (file_exists($staged)) {
                rename($staged, $targetDir . '/' . $file);
            }
        }

        // autoload.php lives in vendor root, not vendor/composer
        $staged = $vendorDir . '/autoload.php' . self::STAGING_SUFFIX;
        if (file_exists($staged)) {
            rename($staged, $vendorDir . '/autoload.php');
        }
    }

    /**
     * Clean up staged files on failure.
     */
    private function cleanStagedFiles(string $targetDir, string $vendorDir): void
    {
        foreach (self::STAGED_TARGET_FILES as $file) {
            $staged = $targetDir . '/' . $file . self::STAGING_SUFFIX;
            if (file_exists($staged)) {
                unlink($staged);
            }
        }

        $staged = $vendorDir . '/autoload.php' . self::STAGING_SUFFIX;
        if (file_exists($staged)) {
            unlink($staged);
        }
    }

    private function buildPayload(
        string $projectDir,
        string $vendorDir,
        InstalledRepositoryInterface $localRepo,
        RootPackageInterface $rootPackage,
        InstallationManager $installationManager,
    ): array {
        $packages = $localRepo->getCanonicalPackages();

        $psr4 = [];
        $psr0 = [];
        $classmap = [];
        $files = [];

        foreach ($packages as $package) {
            if ($package instanceof AliasPackage) {
                continue;
            }
            $installPath = $installationManager->getInstallPath($package);
            if ($installPath === null) {
                continue;
            }
            $entries = $this->collectAutoloadEntries($package->getAutoload(), $installPath, $package);
            $psr4 = array_merge($psr4, $entries['psr4']);
            $psr0 = array_merge($psr0, $entries['psr0']);
            $classmap = array_merge($classmap, $entries['classmap']);
            $files = array_merge($files, $entries['files']);
        }

        $autoloads = [$rootPackage->getAutoload()];
        if ($this->turboDevMode) {
            $autoloads[] = $rootPackage->getDevAutoload();
        }

        foreach ($autoloads as $autoload) {
            $entries = $this->collectAutoloadEntries($autoload, $projectDir, $rootPackage);
            $psr4 = array_merge($psr4, $entries['psr4']);
            $psr0 = array_merge($psr0, $entries['psr0']);
            $classmap = array_merge($classmap, $entries['classmap']);
            $files = array_merge($files, $entries['files']);
        }

        $installedVersionsPath = $vendorDir . '/composer/InstalledVersions.php';
        if (file_exists($installedVersionsPath)) {
            $classmap[] = $installedVersionsPath;
        }

        return [
            'command' => 'classmap',
            'project_dir' => $projectDir,
            'vendor_dir' => $vendorDir,
            'autoload' => [
                'psr-4' => $psr4,
                'psr-0' => $psr0,
                'classmap' => $classmap,
                'files' => $files,
            ],
            'exclude_from_classmap' => $rootPackage->getAutoload()['exclude-from-classmap'] ?? [],
        ];
    }

    /**
     * @return array{psr4: list<array{namespace: string, path: string}>, psr0: list<array{namespace: string, path: string}>, classmap: list<string>, files: list<array{identifier: string, path: string}>}
     */
    private function collectAutoloadEntries(array $autoload, string $basePath, mixed $package): array
    {
        $psr4 = [];
        $psr0 = [];
        $classmap = [];
        $files = [];

        if (array_key_exists('psr-4', $autoload)) {
            foreach ($autoload['psr-4'] as $ns => $paths) {
                foreach ((array) $paths as $path) {
                    $psr4[] = [
                        'namespace' => $ns,
                        'path' => rtrim($basePath . '/' . $path, '/'),
                    ];
                }
            }
        }

        if (array_key_exists('psr-0', $autoload)) {
            foreach ($autoload['psr-0'] as $ns => $paths) {
                foreach ((array) $paths as $path) {
                    $psr0[] = [
                        'namespace' => $ns,
                        'path' => rtrim($basePath . '/' . $path, '/'),
                    ];
                }
            }
        }

        if (array_key_exists('classmap', $autoload)) {
            foreach ((array) $autoload['classmap'] as $path) {
                $classmap[] = rtrim($basePath . '/' . $path, '/');
            }
        }

        if (array_key_exists('files', $autoload)) {
            foreach ((array) $autoload['files'] as $path) {
                $files[] = [
                    'identifier' => $this->getFileIdentifier($package, $path),
                    'path' => rtrim($basePath . '/' . $path, '/'),
                ];
            }
        }

        return ['psr4' => $psr4, 'psr0' => $psr0, 'classmap' => $classmap, 'files' => $files];
    }
}
