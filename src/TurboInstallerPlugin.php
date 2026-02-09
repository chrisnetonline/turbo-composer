<?php

declare(strict_types=1);

namespace TurboComposer;

use Composer\Composer;
use Composer\EventDispatcher\EventSubscriberInterface;
use Composer\Installer\PackageEvent;
use Composer\Installer\PackageEvents;
use Composer\IO\IOInterface;
use Composer\Package\PackageInterface;
use Composer\Plugin\PluginInterface;
use Composer\Script\Event;
use Composer\Script\ScriptEvents;

use function array_values;
use function count;
use function file_exists;
use function glob;
use function is_dir;
use function method_exists;
use function microtime;
use function preg_replace;
use function rmdir;
use function round;
use function str_replace;
use function unlink;

class TurboInstallerPlugin implements PluginInterface, EventSubscriberInterface
{
    private Composer $composer;
    private IOInterface $io;
    private ?RustBridge $bridge = null;

    /** @var array<string, array{zip: string, dest: string, name: string}> */
    private array $pendingExtractions = [];

    /** @var array<string, array{path: string, name: string}> */
    private array $pendingCleanups = [];

    /** @var array<string, array{path: string, name: string, algorithm: string, expected_hash: string}> */
    private array $pendingVerifications = [];

    public function activate(Composer $composer, IOInterface $io): void
    {
        $this->composer = $composer;
        $this->io = $io;
        $this->bridge = new RustBridge($composer, $io);

        if (!$this->bridge->mightBeAvailable()) {
            $io->write(
                '<info>turbo-composer:</info> Binary will be downloaded on first use.',
                true,
                IOInterface::VERBOSE,
            );
            return;
        }

        $this->swapAutoloadGenerator();
        $io->write('<info>turbo-composer:</info> Rust acceleration active.');
    }

    public function deactivate(Composer $composer, IOInterface $io): void {}

    public function uninstall(Composer $composer, IOInterface $io): void
    {
        $vendorDir = $composer->getConfig()->get('vendor-dir');
        $binDir = $vendorDir . '/turbo-composer';

        if (!is_dir($binDir)) {
            return;
        }

        $files = glob($binDir . '/*');
        if ($files !== false) {
            foreach ($files as $file) {
                unlink($file);
            }
        }
        rmdir($binDir);
    }

    public static function getSubscribedEvents(): array
    {
        return [
            PackageEvents::PRE_PACKAGE_INSTALL => ['onPrePackageInstall', \PHP_INT_MAX],
            PackageEvents::PRE_PACKAGE_UPDATE => ['onPrePackageRemoval', \PHP_INT_MAX],
            PackageEvents::PRE_PACKAGE_UNINSTALL => ['onPrePackageRemoval', \PHP_INT_MAX],
            ScriptEvents::PRE_INSTALL_CMD => ['onPreInstall', \PHP_INT_MAX],
            ScriptEvents::POST_INSTALL_CMD => ['onPostOperations', \PHP_INT_MAX],
            ScriptEvents::POST_UPDATE_CMD => ['onPostOperations', \PHP_INT_MAX],
            ScriptEvents::PRE_AUTOLOAD_DUMP => ['onPreAutoloadDump', \PHP_INT_MAX],
        ];
    }

    public function onPreAutoloadDump(Event $event): void
    {
        if ($this->composer->getAutoloadGenerator() instanceof TurboAutoloadGenerator) {
            return;
        }

        if (!$this->bridge->isAvailable()) {
            $this->io->writeError('<warning>turbo-composer:</warning> Binary unavailable. '
            . 'Using default Composer autoloader.');
            return;
        }

        $this->swapAutoloadGenerator();
        $this->io->write('<info>turbo-composer:</info> Rust acceleration active (late init).');
    }

    public function onPrePackageInstall(PackageEvent $event): void
    {
        if ($this->bridge === null || !$this->bridge->mightBeAvailable()) {
            return;
        }

        $operation = $event->getOperation();
        if (!method_exists($operation, 'getPackage')) {
            return;
        }

        $package = $operation->getPackage();
        $cachePath = $this->findCachedArchive($package);
        if ($cachePath === null) {
            return;
        }

        $installPath = $this->composer->getInstallationManager()->getInstallPath($package);
        $this->pendingExtractions[$package->getName()] = [
            'zip' => $cachePath,
            'dest' => $installPath,
            'name' => $package->getName(),
        ];

        $this->queueVerification($package, $cachePath);
    }

    public function onPrePackageRemoval(PackageEvent $event): void
    {
        if ($this->bridge === null || !$this->bridge->mightBeAvailable()) {
            return;
        }

        $package = $this->resolveRemovalPackage($event);
        if ($package === null) {
            return;
        }

        $installPath = $this->composer->getInstallationManager()->getInstallPath($package);
        if ($installPath && is_dir($installPath)) {
            $this->pendingCleanups[$package->getName()] = [
                'path' => $installPath,
                'name' => $package->getName(),
            ];
        }
    }

    public function onPreInstall(Event $event): void
    {
        if ($this->bridge === null || !$this->bridge->isAvailable()) {
            return;
        }

        $locker = $this->composer->getLocker();
        if (!$locker->isLocked()) {
            return;
        }

        $lockData = $locker->getLockData();
        $vendorDir = $this->composer->getConfig()->get('vendor-dir');
        $packages = [];

        foreach ($lockData['packages'] ?? [] as $pkgData) {
            $name = $pkgData['name'] ?? '';
            if ($name === '') {
                continue;
            }
            $packages[] = ['name' => $name, 'install_path' => $vendorDir . '/' . $name];
        }

        if ($packages === []) {
            return;
        }

        $start = microtime(true);
        $result = $this->bridge->run(['command' => 'vendor-check', 'check_packages' => $packages]);
        if ($result === null) {
            return;
        }

        $elapsed = round((microtime(true) - $start) * 1000);
        $present = $result['present'];
        $total = $result['total'];
        $missingCount = count($result['missing']);

        if ($missingCount > 0) {
            $this->io->write(
                "<info>turbo-composer:</info> Vendor check: {$present}/{$total} present, "
                . "{$missingCount} missing ({$elapsed}ms)",
            );
            return;
        }

        $this->io->write(
            "<info>turbo-composer:</info> ✓ Vendor check: all {$total} packages present ({$elapsed}ms)",
            true,
            IOInterface::VERBOSE,
        );
    }

    public function onPostOperations(Event $event): void
    {
        if ($this->bridge === null || !$this->bridge->isAvailable()) {
            $this->pendingCleanups = [];
            $this->pendingVerifications = [];
            $this->pendingExtractions = [];
            return;
        }

        $this->dispatchBatch('clean', 'targets', $this->pendingCleanups);
        $this->dispatchBatch('verify', 'verify_targets', $this->pendingVerifications);
        $this->dispatchBatch('extract', 'packages', $this->pendingExtractions);
    }

    /**
     * Dispatch a batch of pending operations to the Rust binary.
     *
     * @param array<string, mixed> $queue
     */
    private function dispatchBatch(string $command, string $payloadKey, array &$queue): void
    {
        if ($queue === []) {
            return;
        }

        $count = count($queue);
        $start = microtime(true);
        $result = $this->bridge->run([
            'command' => $command,
            $payloadKey => array_values($queue),
        ]);

        $queue = [];

        if ($result === null) {
            $this->io->writeError(
                "<warning>turbo-composer:</warning> Parallel {$command} failed — Composer will handle normally.",
            );
            return;
        }

        $elapsed = round((microtime(true) - $start) * 1000);
        $this->logBatchResult($command, $result, $count, $elapsed);
    }

    private function logBatchResult(string $command, array $result, int $count, float $elapsed): void
    {
        match ($command) {
            'clean' => $this->io->write(
                "<info>turbo-composer:</info> ✓ {$result['cleaned']} directories cleaned in {$elapsed}ms",
                true,
                IOInterface::VERBOSE,
            ),
            'verify' => $this->io->write(
                "<info>turbo-composer:</info> ✓ {$result['verified']}/{$count} archives verified in {$elapsed}ms",
                true,
                IOInterface::VERBOSE,
            ),
            'extract' => $this->io->write(
                "<info>turbo-composer:</info> ✓ {$result['extracted']} packages ({$result['total_files']} files) in {$elapsed}ms",
            ),
            default => null,
        };

        foreach ($result['failed'] ?? [] as $f) {
            $this->io->writeError("<warning>turbo-composer:</warning> {$command} failed: {$f['name']}: {$f['error']}");
        }
    }

    private function queueVerification(PackageInterface $package, string $cachePath): void
    {
        $sha1 = $package->getDistSha1Checksum();
        if (!$sha1) {
            return;
        }

        $this->pendingVerifications[$package->getName()] = [
            'path' => $cachePath,
            'name' => $package->getName(),
            'algorithm' => 'sha1',
            'expected_hash' => $sha1,
        ];
    }

    private function resolveRemovalPackage(PackageEvent $event): ?PackageInterface
    {
        $operation = $event->getOperation();

        if (method_exists($operation, 'getInitialPackage')) {
            return $operation->getInitialPackage();
        }

        if (method_exists($operation, 'getPackage')) {
            return $operation->getPackage();
        }

        return null;
    }

    private function swapAutoloadGenerator(): void
    {
        $generator = new TurboAutoloadGenerator($this->composer->getEventDispatcher(), $this->io, $this->bridge);
        $this->composer->setAutoloadGenerator($generator);
    }

    private function findCachedArchive(PackageInterface $package): ?string
    {
        $cacheDir = $this->composer->getConfig()->get('cache-files-dir');
        $reference = $package->getDistReference() ?? $package->getSourceReference();

        if (!$reference || !$cacheDir) {
            return null;
        }

        $cacheKey = preg_replace(
            '{[^a-z0-9.]}i',
            '-',
            $package->getName() . '/' . $package->getVersion() . '-' . $reference,
        );

        foreach (['.zip', '.tar.gz', '.tar'] as $ext) {
            $path = "{$cacheDir}/{$cacheKey}{$ext}";
            if (file_exists($path)) {
                return $path;
            }
        }

        $glob = $cacheDir . '/' . str_replace('/', '-', $package->getName()) . '/*.zip';
        $matches = glob($glob);
        return $matches[0] ?? null;
    }
}
