<?php

declare(strict_types=1);

namespace TurboComposer\Tests\Unit;

use Composer\Composer;
use Composer\Config;
use Composer\IO\IOInterface;
use Composer\Package\RootPackageInterface;
use Composer\Repository\InstalledRepositoryInterface;
use Composer\Repository\RepositoryManager;
use Composer\Util\HttpDownloader;
use Composer\Util\Loop;
use PHPUnit\Framework\MockObject\Stub;
use PHPUnit\Framework\TestCase;
use TurboComposer\RustBridge;

use function chmod;
use function file_put_contents;
use function is_dir;
use function is_link;
use function mkdir;
use function rmdir;
use function sys_get_temp_dir;
use function uniqid;
use function unlink;

class RustBridgeTest extends TestCase
{
    private string $tempDir;
    private string $noFallbackDir;
    private Composer&Stub $composer;
    private IOInterface&Stub $io;

    protected function setUp(): void
    {
        if (PHP_OS_FAMILY === 'Windows') {
            $this->markTestSkipped(
                'RustBridge tests use shell scripts as fake binaries and are not supported on Windows.',
            );
        }

        $this->tempDir = sys_get_temp_dir() . '/turbo-composer-test-' . uniqid();
        $this->noFallbackDir = $this->tempDir . '/no-fallback';
        mkdir($this->tempDir . '/vendor', 0o755, true);

        $config = $this->createStub(Config::class);
        $config
            ->method('get')
            ->willReturnCallback(fn(string $key) => match ($key) {
                'vendor-dir' => $this->tempDir . '/vendor',
                default => null,
            });

        $rootPackage = $this->createStub(RootPackageInterface::class);
        $rootPackage
            ->method('getExtra')
            ->willReturn([
                'turbo-composer' => [
                    'version' => '0.1.0',
                    'base-url' => 'https://example.invalid',
                ],
            ]);

        $localRepo = $this->createStub(InstalledRepositoryInterface::class);
        $localRepo->method('findPackages')->willReturn([]);

        $repoManager = $this->createStub(RepositoryManager::class);
        $repoManager->method('getLocalRepository')->willReturn($localRepo);

        $this->composer = $this->createStub(Composer::class);
        $this->composer->method('getConfig')->willReturn($config);
        $this->composer->method('getPackage')->willReturn($rootPackage);
        $this->composer->method('getRepositoryManager')->willReturn($repoManager);

        // Stub getLoop so download attempts fail gracefully via the catch block
        $httpDownloader = $this->createStub(HttpDownloader::class);
        $httpDownloader->method('copy')->willThrowException(new \Exception('Download unavailable in tests'));

        $loop = $this->createStub(Loop::class);
        $loop->method('getHttpDownloader')->willReturn($httpDownloader);

        $this->composer->method('getLoop')->willReturn($loop);

        $this->io = $this->createStub(IOInterface::class);
    }

    protected function tearDown(): void
    {
        if (($this->tempDir ?? null) !== null) {
            $this->removeDirectory($this->tempDir);
        }
    }

    public function testMightBeAvailableReturnsFalseWhenNoBinary(): void
    {
        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $this->assertFalse($bridge->mightBeAvailable());
    }

    public function testMightBeAvailableReturnsTrueWithValidBinary(): void
    {
        $this->placeFakeBinary();

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $this->assertTrue($bridge->mightBeAvailable());
    }

    public function testIsAvailableReturnsFalseWhenBinaryCannotBeObtained(): void
    {
        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $this->assertFalse($bridge->isAvailable());
    }

    public function testIsAvailableReturnsTrueWithValidBinary(): void
    {
        $this->placeFakeBinary();

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $this->assertTrue($bridge->isAvailable());
    }

    public function testIsAvailableCachesResult(): void
    {
        $this->placeFakeBinary();

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $this->assertTrue($bridge->isAvailable());
        // Second call should use cached result, not re-resolve
        $this->assertTrue($bridge->isAvailable());
    }

    public function testRunReturnsNullWhenBinaryUnavailable(): void
    {
        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);

        $result = $bridge->run(['command' => 'test']);

        $this->assertNull($result);
    }

    public function testRunReturnsDecodedJsonFromBinary(): void
    {
        $this->placeFakeBinary(output: '{"classmap_count":42,"stats":{}}');

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run(['command' => 'classmap']);

        $this->assertIsArray($result);
        $this->assertSame(42, $result['classmap_count']);
        $this->assertArrayHasKey('stats', $result);
    }

    public function testRunReturnsNullOnNonZeroExit(): void
    {
        $this->placeFakeBinary(exitCode: 1);

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run(['command' => 'test']);

        $this->assertNull($result);
    }

    public function testRunReturnsNullOnInvalidJsonOutput(): void
    {
        $this->placeFakeBinary(output: 'this is not valid json {{{');

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run(['command' => 'test']);

        $this->assertNull($result);
    }

    public function testRunPassesPayloadToBinaryStdin(): void
    {
        // Fake binary that echoes back whatever it receives on stdin as a JSON wrapper
        $this->placeFakeBinaryWithEcho();

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run(['command' => 'classmap', 'project_dir' => '/test']);

        $this->assertIsArray($result);
        $this->assertSame('classmap', $result['command']);
        $this->assertSame('/test', $result['project_dir']);
    }

    /**
     * Place a fake shell script at the expected binary path.
     *
     * The script responds to --version with the configured version,
     * and otherwise consumes stdin then writes the configured output.
     */
    private function placeFakeBinary(
        string $version = '0.1.0',
        string $output = '{"test":true,"stats":{}}',
        int $exitCode = 0,
    ): void {
        $binDir = $this->tempDir . '/vendor/turbo-composer';
        if (!is_dir($binDir)) {
            mkdir($binDir, 0o755, true);
        }

        $script = <<<BASH
            #!/bin/bash
            if [ "\$1" = "--version" ] || [ "\$1" = "-V" ]; then
                echo "turbo-composer {$version}"
                exit 0
            fi
            cat > /dev/null
            printf '%s' '{$output}'
            exit {$exitCode}
            BASH;

        $binaryPath = $binDir . '/turbo-composer';
        file_put_contents($binaryPath, $script);
        chmod($binaryPath, 0o755);
    }

    /**
     * Place a fake binary that reads stdin JSON and echoes it back as stdout.
     */
    private function placeFakeBinaryWithEcho(): void
    {
        $binDir = $this->tempDir . '/vendor/turbo-composer';
        if (!is_dir($binDir)) {
            mkdir($binDir, 0o755, true);
        }

        $script = <<<'BASH'
            #!/bin/bash
            if [ "$1" = "--version" ] || [ "$1" = "-V" ]; then
                echo "turbo-composer 0.1.0"
                exit 0
            fi
            cat
            BASH;

        $binaryPath = $binDir . '/turbo-composer';
        file_put_contents($binaryPath, $script);
        chmod($binaryPath, 0o755);
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
            if (is_link($path) || !$item->isDir()) {
                unlink($path);
                continue;
            }
            rmdir($path);
        }

        rmdir($dir);
    }
}
