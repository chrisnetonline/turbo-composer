<?php

declare(strict_types=1);

namespace TurboComposer\Tests\Unit;

use Composer\EventDispatcher\EventDispatcher;
use Composer\Installer\InstallationManager;
use Composer\IO\IOInterface;
use Composer\Package\AliasPackage;
use Composer\Package\CompletePackage;
use Composer\Package\RootPackageInterface;
use Composer\Repository\InstalledRepositoryInterface;
use PHPUnit\Framework\TestCase;
use ReflectionMethod;
use TurboComposer\RustBridge;
use TurboComposer\TurboAutoloadGenerator;

use function is_dir;
use function is_link;
use function mkdir;
use function rmdir;
use function sys_get_temp_dir;
use function uniqid;
use function unlink;

class TurboAutoloadGeneratorTest extends TestCase
{
    private string $tempDir;
    private TurboAutoloadGenerator $generator;
    private ReflectionMethod $buildPayload;

    protected function setUp(): void
    {
        $this->tempDir = sys_get_temp_dir() . '/turbo-autoload-test-' . uniqid();
        mkdir($this->tempDir . '/vendor/composer', 0o755, true);

        $eventDispatcher = $this->createStub(EventDispatcher::class);
        $io = $this->createStub(IOInterface::class);
        $bridge = $this->createStub(RustBridge::class);

        $this->generator = new TurboAutoloadGenerator($eventDispatcher, $io, $bridge);

        $this->buildPayload = new ReflectionMethod(TurboAutoloadGenerator::class, 'buildPayload');
    }

    protected function tearDown(): void
    {
        if (($this->tempDir ?? null) !== null) {
            $this->removeDirectory($this->tempDir);
        }
    }

    public function testSkipsMetapackagesWithNullInstallPath(): void
    {
        $regularPackage = new CompletePackage('vendor/regular', '1.0.0.0', '1.0.0');
        $regularPackage->setAutoload(['psr-4' => ['Vendor\\Regular\\' => 'src/']]);

        $metaPackage = new CompletePackage('vendor/metapackage', '1.0.0.0', '1.0.0');
        $metaPackage->setAutoload([]);
        $metaPackage->setType('metapackage');

        $localRepo = $this->createStub(InstalledRepositoryInterface::class);
        $localRepo->method('getCanonicalPackages')->willReturn([$regularPackage, $metaPackage]);

        $installationManager = $this->createStub(InstallationManager::class);
        $installationManager
            ->method('getInstallPath')
            ->willReturnCallback(fn($pkg) => match ($pkg->getName()) {
                'vendor/regular' => $this->tempDir . '/vendor/vendor/regular',
                'vendor/metapackage' => null,
                default => null,
            });

        $rootPackage = $this->createStub(RootPackageInterface::class);
        $rootPackage->method('getAutoload')->willReturn([]);
        $rootPackage->method('getDevAutoload')->willReturn([]);

        $payload = $this->buildPayload->invoke(
            $this->generator,
            $this->tempDir,
            $this->tempDir . '/vendor',
            $localRepo,
            $rootPackage,
            $installationManager,
        );

        $psr4Namespaces = array_column($payload['autoload']['psr-4'], 'namespace');
        $this->assertContains('Vendor\\Regular\\', $psr4Namespaces);
        $this->assertCount(1, $payload['autoload']['psr-4']);
    }

    public function testSkipsAliasPackages(): void
    {
        $regularPackage = new CompletePackage('vendor/regular', '1.0.0.0', '1.0.0');
        $regularPackage->setAutoload(['psr-4' => ['Vendor\\Regular\\' => 'src/']]);

        $aliasPackage = new AliasPackage($regularPackage, '1.0.x-dev', '1.0.x-dev');

        $localRepo = $this->createStub(InstalledRepositoryInterface::class);
        $localRepo->method('getCanonicalPackages')->willReturn([$regularPackage, $aliasPackage]);

        $installationManager = $this->createStub(InstallationManager::class);
        $installationManager->method('getInstallPath')->willReturn($this->tempDir . '/vendor/vendor/regular');

        $rootPackage = $this->createStub(RootPackageInterface::class);
        $rootPackage->method('getAutoload')->willReturn([]);
        $rootPackage->method('getDevAutoload')->willReturn([]);

        $payload = $this->buildPayload->invoke(
            $this->generator,
            $this->tempDir,
            $this->tempDir . '/vendor',
            $localRepo,
            $rootPackage,
            $installationManager,
        );

        $this->assertCount(1, $payload['autoload']['psr-4']);
        $this->assertSame('Vendor\\Regular\\', $payload['autoload']['psr-4'][0]['namespace']);
    }

    public function testCollectsAllAutoloadTypes(): void
    {
        $package = new CompletePackage('vendor/full', '1.0.0.0', '1.0.0');
        $package->setAutoload([
            'psr-4' => ['Vendor\\Full\\' => 'src/'],
            'psr-0' => ['Vendor_Legacy_' => 'lib/'],
            'classmap' => ['classes/'],
            'files' => ['helpers.php'],
        ]);

        $localRepo = $this->createStub(InstalledRepositoryInterface::class);
        $localRepo->method('getCanonicalPackages')->willReturn([$package]);

        $installationManager = $this->createStub(InstallationManager::class);
        $installationManager->method('getInstallPath')->willReturn($this->tempDir . '/vendor/vendor/full');

        $rootPackage = $this->createStub(RootPackageInterface::class);
        $rootPackage->method('getAutoload')->willReturn([]);
        $rootPackage->method('getDevAutoload')->willReturn([]);

        $payload = $this->buildPayload->invoke(
            $this->generator,
            $this->tempDir,
            $this->tempDir . '/vendor',
            $localRepo,
            $rootPackage,
            $installationManager,
        );

        $this->assertCount(1, $payload['autoload']['psr-4']);
        $this->assertSame('Vendor\\Full\\', $payload['autoload']['psr-4'][0]['namespace']);

        $this->assertCount(1, $payload['autoload']['psr-0']);
        $this->assertSame('Vendor_Legacy_', $payload['autoload']['psr-0'][0]['namespace']);

        $this->assertCount(1, $payload['autoload']['classmap']);
        $this->assertStringContainsString('classes', $payload['autoload']['classmap'][0]);

        $this->assertCount(1, $payload['autoload']['files']);
        $this->assertStringContainsString('helpers.php', $payload['autoload']['files'][0]['path']);
    }

    public function testIncludesRootPackageAutoload(): void
    {
        $localRepo = $this->createStub(InstalledRepositoryInterface::class);
        $localRepo->method('getCanonicalPackages')->willReturn([]);

        $installationManager = $this->createStub(InstallationManager::class);

        $rootPackage = $this->createStub(RootPackageInterface::class);
        $rootPackage
            ->method('getAutoload')
            ->willReturn([
                'psr-4' => ['App\\' => 'app/'],
            ]);
        $rootPackage
            ->method('getDevAutoload')
            ->willReturn([
                'psr-4' => ['Tests\\' => 'tests/'],
            ]);

        // Without dev mode, only the main autoload should be included
        $payload = $this->buildPayload->invoke(
            $this->generator,
            $this->tempDir,
            $this->tempDir . '/vendor',
            $localRepo,
            $rootPackage,
            $installationManager,
        );

        $namespaces = array_column($payload['autoload']['psr-4'], 'namespace');
        $this->assertContains('App\\', $namespaces);
        $this->assertNotContains('Tests\\', $namespaces);

        // With dev mode, both should be included
        $this->generator->setDevMode(true);

        $payload = $this->buildPayload->invoke(
            $this->generator,
            $this->tempDir,
            $this->tempDir . '/vendor',
            $localRepo,
            $rootPackage,
            $installationManager,
        );

        $namespaces = array_column($payload['autoload']['psr-4'], 'namespace');
        $this->assertContains('App\\', $namespaces);
        $this->assertContains('Tests\\', $namespaces);
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
