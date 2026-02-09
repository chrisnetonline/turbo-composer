<?php

declare(strict_types=1);

namespace TurboComposer;

use Composer\Composer;
use Composer\IO\IOInterface;

use function fclose;
use function fwrite;
use function is_resource;
use function json_decode;
use function json_encode;
use function proc_close;
use function proc_open;
use function stream_get_contents;
use function trim;

use const JSON_THROW_ON_ERROR;
use const JSON_UNESCAPED_SLASHES;
use const JSON_UNESCAPED_UNICODE;

class RustBridge
{
    private Composer $composer;
    private IOInterface $io;
    private ?string $fallbackDir;

    private null|false|string $binaryPath = null;
    private bool $resolved = false;

    public function __construct(Composer $composer, IOInterface $io, ?string $fallbackDir = null)
    {
        $this->composer = $composer;
        $this->io = $io;
        $this->fallbackDir = $fallbackDir;
    }

    public function mightBeAvailable(): bool
    {
        if ($this->resolved) {
            return $this->binaryPath !== false;
        }

        return (new BinaryInstaller($this->composer, $this->io, $this->fallbackDir))->existsLocally();
    }

    public function isAvailable(): bool
    {
        $this->resolve();
        return $this->binaryPath !== false;
    }

    public function run(array $payload): ?array
    {
        $collect = $this->startAsync($payload);
        return $collect !== null ? $collect() : null;
    }

    /**
     * Start the Rust binary asynchronously and return a callable that collects
     * the result. This allows the caller to do other work while Rust runs.
     *
     * @return (callable(): ?array)|null  Returns null if the binary cannot start.
     */
    public function startAsync(array $payload): ?callable
    {
        $this->resolve();

        if ($this->binaryPath === false) {
            return null;
        }

        $json = json_encode($payload, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_THROW_ON_ERROR);

        $descriptors = [
            0 => ['pipe', 'r'],
            1 => ['pipe', 'w'],
            2 => ['pipe', 'w'],
        ];

        $proc = proc_open([$this->binaryPath], $descriptors, $pipes);
        if (!is_resource($proc)) {
            $this->io->writeError('<warning>turbo-composer:</warning> Failed to start binary.');
            return null;
        }

        fwrite($pipes[0], $json);
        fclose($pipes[0]);

        $io = $this->io;

        return static function () use ($proc, $pipes, $io): ?array {
            $stdout = stream_get_contents($pipes[1]);
            $stderr = stream_get_contents($pipes[2]);
            fclose($pipes[1]);
            fclose($pipes[2]);

            $exit = proc_close($proc);

            if ($stderr !== '' && $stderr !== false) {
                $io->write('<info>turbo-composer:</info> ' . trim($stderr), true, IOInterface::VERBOSE);
            }

            if ($exit !== 0) {
                $io->writeError("<warning>turbo-composer:</warning> Binary failed (exit {$exit}): {$stderr}");
                return null;
            }

            try {
                return json_decode($stdout, associative: true, flags: JSON_THROW_ON_ERROR);
            } catch (\JsonException $e) {
                $io->writeError(
                    '<warning>turbo-composer:</warning> Could not parse binary output as JSON: ' . $e->getMessage(),
                );
                return null;
            }
        };
    }

    private function resolve(): void
    {
        if ($this->resolved) {
            return;
        }
        $this->resolved = true;

        $path = (new BinaryInstaller($this->composer, $this->io, $this->fallbackDir))->ensure();
        $this->binaryPath = $path ?? false;
    }
}
