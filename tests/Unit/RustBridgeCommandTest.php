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

class RustBridgeCommandTest extends TestCase
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

    public function testRunCleanCommandReturnsResult(): void
    {
        $this->placeFakeBinary(output: '{"cleaned":3,"failed":[],"elapsed_ms":12}');

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'clean',
            'targets' => [
                ['path' => '/tmp/pkg1', 'name' => 'vendor/pkg1'],
                ['path' => '/tmp/pkg2', 'name' => 'vendor/pkg2'],
                ['path' => '/tmp/pkg3', 'name' => 'vendor/pkg3'],
            ],
        ]);

        $this->assertIsArray($result);
        $this->assertSame(3, $result['cleaned']);
        $this->assertSame([], $result['failed']);
    }

    public function testRunClassmapReportsWalkSkipped(): void
    {
        $this->placeFakeBinary(
            output: '{"classmap_count":10,"files_written":false,"stats":{"walk_skipped":true,"walk_ms":5}}',
        );

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run(['command' => 'classmap']);

        $this->assertIsArray($result);
        $this->assertSame(10, $result['classmap_count']);
        $this->assertTrue($result['stats']['walk_skipped']);
    }

    public function testRunVerifyCommandReturnsResult(): void
    {
        $this->placeFakeBinary(output: '{"verified":5,"failed":[],"total":5,"elapsed_ms":3}');

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'verify',
            'verify_targets' => [
                [
                    'path' => '/tmp/pkg1.zip',
                    'name' => 'vendor/pkg1',
                    'algorithm' => 'sha256',
                    'expected_hash' => 'abc123',
                ],
                [
                    'path' => '/tmp/pkg2.zip',
                    'name' => 'vendor/pkg2',
                    'algorithm' => 'sha256',
                    'expected_hash' => 'def456',
                ],
            ],
        ]);

        $this->assertIsArray($result);
        $this->assertSame(5, $result['verified']);
        $this->assertSame(5, $result['total']);
        $this->assertSame([], $result['failed']);
    }

    public function testRunVendorCheckCommandReturnsResult(): void
    {
        $this->placeFakeBinary(
            output: '{"present":10,"missing":["vendor/missing"],"incomplete":[],"total":11,"elapsed_ms":1}',
        );

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'vendor-check',
            'check_packages' => [
                ['name' => 'vendor/pkg1', 'install_path' => '/tmp/vendor/pkg1'],
            ],
        ]);

        $this->assertIsArray($result);
        $this->assertSame(10, $result['present']);
        $this->assertSame(11, $result['total']);
        $this->assertSame(['vendor/missing'], $result['missing']);
        $this->assertSame([], $result['incomplete']);
    }

    public function testRunVerifyCommandWithFailuresReturnsDetails(): void
    {
        $this->placeFakeBinary(
            output: '{"verified":1,"failed":[{"name":"vendor/bad","expected":"abc","actual":"def"}],"total":2,"elapsed_ms":5}',
        );

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'verify',
            'verify_targets' => [],
        ]);

        $this->assertIsArray($result);
        $this->assertSame(1, $result['verified']);
        $this->assertSame(2, $result['total']);
        $this->assertCount(1, $result['failed']);
        $this->assertSame('vendor/bad', $result['failed'][0]['name']);
    }

    public function testRunVendorCheckWithIncompletePackages(): void
    {
        $this->placeFakeBinary(
            output: '{"present":5,"missing":[],"incomplete":["vendor/empty"],"total":6,"elapsed_ms":2}',
        );

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'vendor-check',
            'check_packages' => [],
        ]);

        $this->assertIsArray($result);
        $this->assertSame(5, $result['present']);
        $this->assertSame([], $result['missing']);
        $this->assertSame(['vendor/empty'], $result['incomplete']);
    }

    public function testRunBatchCommandReturnsResults(): void
    {
        $batchOutput = json_encode([
            'results' => [
                ['command' => 'clean', 'result' => ['cleaned' => 2, 'failed' => [], 'elapsed_ms' => 3]],
                ['command' => 'verify', 'result' => ['verified' => 5, 'failed' => [], 'total' => 5, 'elapsed_ms' => 2]],
            ],
            'elapsed_ms' => 8,
        ]);

        $this->placeFakeBinary(output: $batchOutput);

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'batch',
            'operations' => [
                ['command' => 'clean', 'targets' => []],
                ['command' => 'verify', 'verify_targets' => []],
            ],
        ]);

        $this->assertIsArray($result);
        $this->assertArrayHasKey('results', $result);
        $this->assertCount(2, $result['results']);
        $this->assertSame('clean', $result['results'][0]['command']);
        $this->assertSame(2, $result['results'][0]['result']['cleaned']);
        $this->assertSame('verify', $result['results'][1]['command']);
        $this->assertSame(5, $result['results'][1]['result']['verified']);
    }

    public function testRunClassmapWithStagingSuffixOmitsContents(): void
    {
        $this->placeFakeBinary(
            output: '{"classmap_count":42,"files_written":true,"stats":{"walk_skipped":false,"walk_ms":10,"generate_ms":2}}',
        );

        $bridge = new RustBridge($this->composer, $this->io, $this->noFallbackDir);
        $result = $bridge->run([
            'command' => 'classmap',
            'staging_suffix' => '.turbo',
        ]);

        $this->assertIsArray($result);
        $this->assertSame(42, $result['classmap_count']);
        $this->assertTrue($result['files_written']);
        // When using staging, Rust omits file contents from JSON response
        $this->assertArrayNotHasKey('classmap_file_content', $result);
    }

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
