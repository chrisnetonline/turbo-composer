<?php

declare(strict_types=1);

namespace TurboComposer\Tests;

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
use TurboComposer\BinaryInstaller;

use function chmod;
use function file_put_contents;
use function is_dir;
use function is_link;
use function mkdir;
use function rmdir;
use function sys_get_temp_dir;
use function uniqid;
use function unlink;

class BinaryInstallerTest extends TestCase
{
    private string $tempDir;
    private Composer&Stub $composer;
    private IOInterface&Stub $io;

    protected function setUp(): void
    {
        if (PHP_OS_FAMILY === 'Windows') {
            $this->markTestSkipped(
                'BinaryInstaller tests use shell scripts as fake binaries and are not supported on Windows.',
            );
        }

        $this->tempDir = sys_get_temp_dir() . '/turbo-composer-test-' . uniqid();
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

        $this->io = $this->createStub(IOInterface::class);
    }

    protected function tearDown(): void
    {
        if (($this->tempDir ?? null) !== null) {
            $this->removeDirectory($this->tempDir);
        }
    }

    public function testExistsLocallyReturnsFalseWhenNoBinaryExists(): void
    {
        $installer = $this->createInstaller();

        $this->assertFalse($installer->existsLocally());
    }

    public function testExistsLocallyReturnsTrueWithValidBinary(): void
    {
        $this->placeFakeBinary();

        $installer = $this->createInstaller();

        $this->assertTrue($installer->existsLocally());
    }

    public function testExistsLocallyReturnsFalseWhenVersionMismatch(): void
    {
        $this->placeFakeBinary(version: '9.9.9');

        $installer = $this->createInstaller();

        $this->assertFalse($installer->existsLocally());
    }

    public function testExistsLocallyReturnsFalseWhenNotExecutable(): void
    {
        $binDir = $this->tempDir . '/vendor/turbo-composer';
        mkdir($binDir, 0o755, true);
        file_put_contents($binDir . '/turbo-composer', "#!/bin/bash\necho 'turbo-composer 0.1.0'\n");
        // Intentionally NOT setting executable permission

        $installer = $this->createInstaller();

        $this->assertFalse($installer->existsLocally());
    }

    public function testEnsureReturnsPathWhenBinaryAlreadyValid(): void
    {
        $this->placeFakeBinary();

        $installer = $this->createInstaller();
        $path = $installer->ensure();

        $this->assertNotNull($path);
        $this->assertStringContainsString('turbo-composer', $path);
    }

    public function testEnsureReturnsNullWhenBinaryCannotBeObtained(): void
    {
        $this->mockFailedDownload();

        $installer = $this->createInstaller();
        $result = $installer->ensure();

        $this->assertNull($result);
    }

    public function testEnsureDoesNotReturnStaleVersionBinary(): void
    {
        // Place a binary with the wrong version — ensure() should not return it
        // and should try downloading instead (which will fail in tests)
        $this->placeFakeBinary(version: '0.0.1');
        $this->mockFailedDownload();

        $installer = $this->createInstaller();
        $result = $installer->ensure();

        $this->assertNull($result);
    }

    private function createInstaller(): BinaryInstaller
    {
        // Use a non-existent fallback dir so tests don't pick up locally-built binaries
        return new BinaryInstaller($this->composer, $this->io, $this->tempDir . '/no-fallback');
    }

    private function placeFakeBinary(string $version = '0.1.0'): void
    {
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
            echo '{"test":true}'
            BASH;

        $binaryPath = $binDir . '/turbo-composer';
        file_put_contents($binaryPath, $script);
        chmod($binaryPath, 0o755);
    }

    private function mockFailedDownload(): void
    {
        $httpDownloader = $this->createStub(HttpDownloader::class);
        $httpDownloader->method('copy')->willThrowException(new \Exception('Download failed: connection refused'));

        $loop = $this->createStub(Loop::class);
        $loop->method('getHttpDownloader')->willReturn($httpDownloader);

        $this->composer->method('getLoop')->willReturn($loop);
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
