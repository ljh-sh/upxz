# upxz

[![CII Best Practices](https://bestpractices.coreinfrastructure.org/projects/0/badge)](https://bestpractices.coreinfrastructure.org/projects/0)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ljh-sh/upxz/badge)](https://scorecard.dev/viewer/?uri=github.com/ljh-sh/upxz)

> Tiny single-binary file packer. One file in, one file out, magic-checked, zstd-packed.

> 极简单二进制文件打包器：一个文件进、一个文件出，校验文件头 magic，用 zstd 打包。 — [中文文档](README.cn.md)

## What is this

`upxz` wraps a single file in a small container and compresses it with
[zstd](https://datatracker.ietf.org/doc/html/rfc8878). It is deliberately
narrow:

- **One file in, one file out.** No directories, no globs, no batch mode.
- **Magic-checked.** The container starts with an 8-byte magic (`UPXZ\x01…`).
  Re-packing an existing container is refused.
- **Single binary.** Statically links zstd; no Python, no extra runtimes.

Two subcommands:

| command                  | effect                                                |
| ------------------------ | ----------------------------------------------------- |
| `upxz pack <FILE>`       | wrap `FILE` as `FILE.upxz` (magic + original name + zstd payload) |
| `upxz unpack <FILE.upxz>` | verify magic, restore the original bytes to disk      |

## Install

```bash
# from source
cargo install --git https://github.com/ljh-sh/upxz

# or download a prebuilt binary from releases
# https://github.com/ljh-sh/upxz/releases
```

## Usage

```bash
# pack with the default compression tier (zstd level 3)
upxz pack notes.txt              # -> notes.txt.upxz

# trade CPU for a smaller file
upxz pack notes.txt --best -o notes.txt.upxz

# lowest CPU, useful in hot loops
upxz pack notes.txt --fast

# restore the original bytes
upxz unpack notes.txt.upxz       # -> notes.txt (name from the container header)
```

### Compression tiers

upxz exposes zstd as three named presets rather than a raw `--level=N` knob:

| tier      | flag     | zstd level | when to use                    |
| --------- | -------- | ---------- | ------------------------------ |
| default   | _(none)_ | 3          | the common case                |
| fast      | `--fast` | 1          | minimize CPU                   |
| best      | `--best` | 19         | smallest output, highest CPU   |

`--fast` and `--best` are mutually exclusive.

## Why zstd only (no xz / liblzma)

`upxz` does not depend on `xz2` / `liblzma`. The container compresses with zstd
only, so the build stays a single binary with a permissive license story
(Apache-2.0 project + BSD-licensed zstd bindings) and no LGPL entanglement.
See [`DESIGN.md`](DESIGN.md) for the full rationale.

## Self-extracting binaries (`-c` / `--create-sfx`)

`upxz -c <orig> -o <packed>` produces a **self-extracting executable**: a single
`packed` file you can `chmod +x` and run directly. Running it decompresses the
original and execs it with the original name as `argv[0]` and all trailing
args forwarded verbatim; the inner program's exit code is propagated.

> The SFX output path is given by `-o`/`--out` (not a second positional), so
> that the trailing-args form `upxz foo.upxz -- -a -b` works unambiguously for
> the runner.

The SFX mechanism is platform-specific:

### Linux — in-memory exec (memfd)

Running the packed binary decompresses the original into a `memfd_create`
memory file and `fexecve`s it — **no temp file is ever written to disk**.

```bash
# build an SFX (Linux)
upxz -c /usr/local/bin/myapp -o ./myapp.sfx

# run it — argv and exit code are transparent
./myapp.sfx --flag value    # forwards --flag value to myapp
echo $?                     # myapp's exit code
```

Layout:

```
[ stub ELF ][ .upxz container (magic+namelen+name+zstd) ][ u64 stub_size BE ]
```

The stub (`stub/` crate) reads `/proc/self/exe`, recovers `stub_size` from the
trailing 8 bytes, slices out the `.upxz` container, decompresses, and execs.
It is compiled and embedded into `upxz` at build time by `build.rs`, so
`upxz -c` needs no separate artifact on disk.

### macOS — two-segment SFX (loader + app)

macOS has no `memfd_create` / in-memory exec, so the SFX is two segments:

```
[ upxz-loader Mach-O (codesigned) ][ .upxz container ][ trailer ]
```

The trailer (last 16 bytes) records each segment's length so the loader can
locate its own slice:

```
b"UPXZEND1" (8) + loader_len (u32 BE) + app_len (u32 BE)
```

Running `./packed` executes the loader directly — the packed file's Mach-O
header IS the loader (codesigned). The kernel reads the `mach_header` at
offset 0 and AMFI accepts the loader's cdhash. The appended app bytes make
`codesign --verify --strict` fail (the file's integrity is perturbed), but
**exec is unaffected**. The loader (a `#![no_std]` binary, ~84 KB) resolves
its own path via `_NSGetExecutablePath`, reads the trailer, slices out the
`.upxz` app segment at offset `loader_len`, zstd-decompresses it, writes the
restored binary to `/tmp/upxz-app-<pid>`, ad-hoc re-signs it (macOS AMFI
SIGKILLs an unsigned restored copy on exec), and `execv`s it.

```bash
# build an SFX (macOS)
upxz -c /usr/local/bin/myapp -o ./myapp.sfx
./myapp.sfx --flag value    # forwards --flag value to myapp
echo $?                     # myapp's exit code
```

Each run leaves a `/tmp/upxz-app-<pid>` behind (the loader cannot `unlink`
after `execv`, and `fork`-ing a cleanup watchdog from the ad-hoc-signed no_std
loader triggers AMFI SIGKILL on the exec'd program). These files are harmless
(owner-only, `chmod 0500`) and are cleared on reboot.

This feature is `#[cfg(target_os = "linux")]` / `#[cfg(target_os = "macos")]`.
Windows keeps the runner model (decompress to a temp file, exec).

## Run a single entry from a `.tar.zst` (`--bin`)

`upxz --bin <inner-path> <archive.tar.zst> [-- args...]` runs **one designated
binary** from a `.tar.zst` archive **without extracting the whole archive**.
This is AppImage-style distribution: the archive is the distribution unit, and
upxz streams it (zstd-decode → tar-parse) and materializes only the matched
entry — into a `memfd` on Linux (never touches disk) or a temp file + ad-hoc
codesign on macOS — then `execve`s it. All other entries are read and
discarded; they are never written to disk.

```bash
# archive layout: bin/myapp, lib/..., share/...
upxz --bin bin/myapp app.tar.zst -- --flag value
#  -> runs bin/myapp with argv = [--flag value], forwards exit code

# leading "./" on the inner path is normalized, so this is equivalent:
upxz --bin ./bin/myapp app.tar.zst
```

`argv[0]` of the inner process is the inner binary's basename; everything after
`--` is forwarded verbatim (including hyphen-leading flags). On Linux the
binary lives only in memory; on macOS it is written to `$TMPDIR`, ad-hoc
codesigned, and removed after the child exits.

## Container format

```
+------------------+----------------------+--------------------------+
| magic (8 bytes)  | name-len (4 bytes BE)| original file name (UTF-8)|
+------------------+----------------------+--------------------------+
+--------------------------------------------------------------------+
| zstd frame (compressed original file bytes)                        |
+--------------------------------------------------------------------+
```

The stored file name is a flat path component (no separators, no `..`); the
unpack step re-validates it before writing, so a tampered container cannot
escape the current directory.

## License

Apache-2.0. The zstd bindings (`zstd` crate) are MIT.
