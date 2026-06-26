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

## Linux self-extracting binaries (`-c` / `--create-sfx`)

On Linux, `upxz -c <orig> <packed>` produces a **self-extracting executable**:
a single `packed` file you can `chmod +x` and run directly. Running it
decompresses the original into a `memfd_create` memory file and `fexecve`s
it — **no temp file is ever written to disk**, and the original runs with its
own name as `argv[0]` and all trailing args forwarded verbatim.

```bash
# build an SFX (Linux only)
upxz -c /usr/local/bin/myapp ./myapp.sfx

# run it — argv and exit code are transparent
./myapp.sfx --flag value    # forwards --flag value to myapp
echo $?                     # myapp's exit code
```

Layout of an SFX file:

```
[ stub ELF ][ .upxz container (magic+namelen+name+zstd) ][ u64 stub_size BE ]
```

The stub reads `/proc/self/exe`, recovers `stub_size` from the trailing 8
bytes, slices out the `.upxz` container, decompresses, and execs. The stub
(`stub/` crate) is compiled and embedded into `upxz` at build time by
`build.rs`, so `upxz -c` needs no separate artifact on disk.

This feature is `#[cfg(target_os = "linux")]` only. macOS/Windows keep the
runner model (decompress to a temp file, exec).

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
