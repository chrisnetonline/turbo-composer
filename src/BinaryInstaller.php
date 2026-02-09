<?php

declare(strict_types=1);

namespace TurboComposer;

use Composer\Composer;
use Composer\IO\IOInterface;

use function chmod;
use function copy;
use function dirname;
use function fclose;
use function file_exists;
use function filesize;
use function is_dir;
use function is_executable;
use function mkdir;
use function proc_close;
use function proc_open;
use function round;
use function str_contains;
use function str_starts_with;
use function stream_get_contents;
use function trim;
use function unlink;

class BinaryInstaller
{
    private const BINARY_NAME = 'turbo-composer';
    private const PACKAGE_NAME = 'chrisnetonline/turbo-composer';
    private const DEFAULT_BASE_URL = 'https://github.com/chrisnetonline/turbo-composer/releases/download';

    private Composer $composer;
    private IOInterface $io;
    private string $fallbackDir;

    public function __construct(Composer $composer, IOInterface $io, ?string $fallbackDir = null)
    {
        $this->composer = $composer;
        $this->io = $io;
        $this->fallbackDir = $fallbackDir ?? dirname(__DIR__) . '/bin';
    }

    public function existsLocally(): bool
    {
        return $this->isValidBinary($this->localBinaryPath());
    }

    public function ensure(): ?string
    {
        $binaryPath = $this->localBinaryPath();
        $binaryName = $this->getPlatformBinaryName();

        if ($binaryName === null) {
            $this->io->writeError(
                '<warning>turbo-composer:</warning> Unsupported platform: ' . PHP_OS_FAMILY . ' ' . php_uname('m'),
            );
            return null;
        }

        if ($this->isValidBinary($binaryPath)) {
            $this->io->write('<info>turbo-composer:</info> Binary already installed.', true, IOInterface::VERBOSE);
            return $binaryPath;
        }

        if ($this->download($binaryName, $binaryPath)) {
            return $binaryPath;
        }

        $fallback = $this->fallbackDir . '/' . $binaryName;
        if (file_exists($fallback)) {
            $this->io->write('<info>turbo-composer:</info> Using bundled fallback binary.');
            if ($this->install($fallback, $binaryPath)) {
                return $binaryPath;
            }
        }

        $this->io->writeError('<warning>turbo-composer:</warning> Could not obtain binary. '
        . 'Plugin will fall back to default Composer behaviour.');

        return null;
    }

    private function download(string $binaryName, string $destPath): bool
    {
        $version = $this->getPluginVersion();
        $baseUrl = $this->getBaseUrl();
        $url = "{$baseUrl}/v{$version}/{$binaryName}";

        $this->io->write("<info>turbo-composer:</info> Downloading binary from {$url}…");

        try {
            $dir = dirname($destPath);
            if (!is_dir($dir)) {
                mkdir($dir, 0o755, true);
            }

            $httpDownloader = $this->composer->getLoop()->getHttpDownloader();
            $tmpFile = $destPath . '.tmp';

            $httpDownloader->copy($url, $tmpFile);

            if (!file_exists($tmpFile) || filesize($tmpFile) < 1024) {
                if (file_exists($tmpFile)) {
                    unlink($tmpFile);
                }
                $this->io->writeError('<warning>turbo-composer:</warning> Download produced an invalid file.');
                return false;
            }

            if (!$this->install($tmpFile, $destPath)) {
                if (file_exists($tmpFile)) {
                    unlink($tmpFile);
                }
                return false;
            }

            if (file_exists($tmpFile)) {
                unlink($tmpFile);
            }

            $size = round((filesize($destPath) / 1024) / 1024, 1);
            $this->io->write("<info>turbo-composer:</info> ✓ Binary installed ({$size} MB)");

            return true;
        } catch (\Exception $e) {
            $tmpPath = $destPath . '.tmp';
            if (file_exists($tmpPath)) {
                unlink($tmpPath);
            }
            $this->io->writeError('<warning>turbo-composer:</warning> Download failed: ' . $e->getMessage());
            return false;
        }
    }

    private function install(string $source, string $dest): bool
    {
        $dir = dirname($dest);
        if (!is_dir($dir)) {
            mkdir($dir, 0o755, true);
        }

        if (!copy($source, $dest)) {
            $this->io->writeError('<warning>turbo-composer:</warning> Failed to copy binary to ' . $dest);
            return false;
        }

        if (PHP_OS_FAMILY !== 'Windows') {
            chmod($dest, 0o755);
        }

        return true;
    }

    private function isValidBinary(string $path): bool
    {
        if (!file_exists($path)) {
            return false;
        }

        if (PHP_OS_FAMILY !== 'Windows' && !is_executable($path)) {
            return false;
        }

        $descriptors = [
            0 => ['pipe', 'r'],
            1 => ['pipe', 'w'],
            2 => ['pipe', 'w'],
        ];

        $proc = proc_open([$path, '--version'], $descriptors, $pipes);
        if (!is_resource($proc)) {
            return false;
        }

        fclose($pipes[0]);
        $stdout = trim(stream_get_contents($pipes[1]));
        fclose($pipes[1]);
        fclose($pipes[2]);

        if (proc_close($proc) !== 0) {
            return false;
        }

        $expected = $this->getPluginVersion();

        // Dev installs (e.g. "dev-main" from a path repo): any working binary is valid
        if (str_starts_with($expected, 'dev-')) {
            return str_contains($stdout, self::BINARY_NAME);
        }

        return str_contains($stdout, $expected);
    }

    private function localBinaryPath(): string
    {
        $vendorDir = $this->composer->getConfig()->get('vendor-dir');
        $binDir = $vendorDir . '/turbo-composer';

        $ext = PHP_OS_FAMILY === 'Windows' ? '.exe' : '';
        return $binDir . '/' . self::BINARY_NAME . $ext;
    }

    /**
     * Resolve the plugin version from Composer's installed packages.
     *
     * Falls back to the root project's extra config, then to the Cargo.toml-defined version.
     */
    private function getPluginVersion(): string
    {
        $repo = $this->composer->getRepositoryManager()->getLocalRepository();
        $packages = $repo->findPackages(self::PACKAGE_NAME);

        if ($packages !== []) {
            $version = $packages[0]->getPrettyVersion();
            // Strip 'v' prefix if present (e.g., "v0.1.0" → "0.1.0")
            return ltrim($version, 'v');
        }

        // Fallback: root extra config (for development / pre-install scenarios)
        $extra = $this->composer->getPackage()->getExtra();
        return $extra['turbo-composer']['version'] ?? '0.1.0';
    }

    private function getBaseUrl(): string
    {
        $extra = $this->composer->getPackage()->getExtra();
        return $extra['turbo-composer']['base-url'] ?? self::DEFAULT_BASE_URL;
    }

    private function getPlatformBinaryName(): ?string
    {
        $os = match (PHP_OS_FAMILY) {
            'Windows' => 'windows',
            'Darwin' => 'darwin',
            'Linux' => 'linux',
            default => null,
        };
        if ($os === null) {
            return null;
        }

        $arch = match (php_uname('m')) {
            'x86_64', 'amd64' => 'x86_64',
            'aarch64', 'arm64' => 'aarch64',
            default => null,
        };
        if ($arch === null) {
            return null;
        }

        $ext = $os === 'windows' ? '.exe' : '';
        return self::BINARY_NAME . "-{$os}-{$arch}{$ext}";
    }
}
