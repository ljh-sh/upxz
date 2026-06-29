# upxz

[![CII Best Practices](https://bestpractices.coreinfrastructure.org/projects/0/badge)](https://bestpractices.coreinfrastructure.org/projects/0)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ljh-sh/upxz/badge)](https://scorecard.dev/viewer/?uri=github.com/ljh-sh/upxz)

> Tiny single-binary file packer. One file in, one file out, magic-checked, zstd-packed.

> 极简单二进制文件打包器：一个文件进、一个文件出，校验文件头 magic，用 zstd 打包。 — [中文文档](README.cn.md)

## TL;DR

```bash
upxz notes.txt               # pack   → notes.txt.upxz (a self-extractor; ./notes.txt.upxz runs)
./notes.txt.upxz             # run    → decompress + exec the original (that IS the run)
upxz notes.txt.upxz          # refused — already packed; run it directly or use -d
upxz -d notes.txt.upxz       # unpack → restore the original (chmod +x for executables)
upxz -l notes.txt.upxz       # list   → codec / sizes / original name
upxz -t notes.txt.upxz       # test   → verify magic + round-trip
upxz -c myapp -o myapp.sfx   # pack to a self-extractor at an explicit output path
upxz --fast big.bin          # pack at zstd level 1 (lowest CPU)
upxz --gz notes.txt          # pack with gzip instead of zstd
```

A plain `FILE` is **packed** into a self-extractor `<FILE>.upxz` — run it
directly. An already-packed file is **refused** (the SFX runs itself; there is
no `upxz run` subcommand, by design). The read paths (`-d`/`-l`/`-t`) all
locate the embedded UPXZ container automatically (bare v0.1–v0.3 containers
and self-extractors share one read path).

## What is this

`upxz` wraps a single file in a small container and compresses it with
[zstd](https://datatracker.ietf.org/doc/html/rfc8878). It is deliberately
narrow:

- **One file in, one file out.** No directories, no globs, no batch mode.
- **upx-style output.** The default pack produces a **self-extracting
  executable** (`<FILE>.upxz`, chmod +x). `./<FILE>.upxz` runs the original
  directly — there is no separate runner step inside upxz itself.
- **Magic-checked.** Re-packing an already-packed file (bare container or
  self-extractor) is refused.
- **Single binary.** Statically links zstd; no Python, no extra runtimes.

upx-style **flat CLI** (no subcommands) — a flag picks the action:

| invocation                       | action                                                        |
| -------------------------------- | ------------------------------------------------------------- |
| `upxz <FILE>`                    | **pack** → `<FILE>.upxz` (self-extractor: stub + container + trailer, chmod +x) |
| `./<FILE>.upxz`                  | **run** → decompress + exec the original (that IS the run; propagates exit code) |
| `upxz <FILE>.upxz`               | **refused** — already packed; run the .upxz directly or use `-d` |
| `upxz -d <FILE>.upxz`            | **unpack** → restore the original (executable bit restored for executables) |
| `upxz -l <FILE>.upxz`            | **list** → codec / sizes / original name (read-only)          |
| `upxz -t <FILE>.upxz`            | **test** → verify magic + round-trip decompress (read-only)   |
| `upxz -c <orig> -o <packed>`     | **pack** → self-extractor at an explicit output path          |
| `upxz --bin <inner> <a.tar.zst>` | **bin run** → run one entry from a `.tar.zst` without extracting |

## Install

**Prebuilt binary** (Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64 — cosign-signed,
with `SHA256SUMS`) from the [latest release](https://github.com/ljh-sh/upxz/releases/latest):

```bash
# pick the tarball for your platform, then:
tar xJf upxz-<target>.tar.xz -C /usr/local/bin --strip-components=1 bin/upxz

# verify the checksum + signature
sha256sum -c SHA256SUMS --ignore-missing
cosign verify-blob --bundle upxz-<target>.tar.xz.sigstore.json upxz-<target>.tar.xz
```

**From source** (full feature set incl. SFX `-c`):

```bash
cargo install --git https://github.com/ljh-sh/upxz
```

> **`cargo install upxz` (from crates.io) gives the packer but not the SFX runtime.**
> A `cargo publish` tarball strips the nested SFX companion crates
> (`stub/`/`loader/`/`winstub/`), so the self-extracting-binary output
> (`<FILE>.upxz`) cannot be built from a crates.io install — `upxz <FILE>`
> refuses with a clear message. `-d`/`-l`/`-t`/`--bin` on a pre-existing SFX
> still work (they read the embedded container without needing the stub).
> For SFX packing, use a release binary or `cargo install --git` (both ship full
> source). Tracked in [#11](https://github.com/ljh-sh/upxz/issues/11).

## Usage

```bash
# pack a file (zstd, default level 19) — produces a self-extractor with chmod +x
upxz notes.txt                   # -> notes.txt.upxz (a self-extractor)

# run the self-extractor directly (that IS the run — upxz has no run mode)
./notes.txt.upxz                 # decompresses + execs the original; propagates exit code
./notes.txt.upxz -- --flag value # args after -- forwarded verbatim to the inner program

# inspect / verify / restore (read-mostly) — all locate the embedded container
upxz -l notes.txt.upxz           # list: codec / sizes / original name
upxz -t notes.txt.upxz           # test: magic + round-trip decompress
upxz -d notes.txt.upxz           # unpack: restore the original bytes (-> notes.txt);
                                 #         chmod +x when the original was an executable
upxz -d notes.txt.upxz -f        # overwrite an existing notes.txt

# pick a compression level
upxz --fast notes.txt            # zstd level 1 — lowest CPU, hot loops
upxz -z 9 notes.txt              # zstd level 9  (any 1..=19)
upxz --gz notes.txt              # gzip instead of zstd (codec id 1 in the embedded magic)

# build a self-extractor at an explicit output path (same as bare pack, but
# pick the output name — useful for renaming or building from a non-suffix path)
upxz -c myapp -o myapp.sfx && ./myapp.sfx --flag value
```

### Compression

A 2-tier preset plus an explicit level — no `--best`, never `-22` (a documented
zstd trap: same size as `-19`, ~2× slower). `-z N` wins over `--fast`:

| selection | flag      | zstd level | when                     |
| --------- | --------- | ---------- | ------------------------ |
| default   | _(none)_  | 19         | the common case (smallest) |
| fast      | `--fast`  | 1          | minimize CPU             |
| explicit  | `-z N`    | N (1..=19) | pin a specific level     |

`--gz` switches the codec to gzip (DEFLATE 1..=9, default 9); `-z N` then sets
the DEFLATE level. See [Codec: zstd or gzip](#codec-zstd-default-or-gzip-gz)
below.

### Codec: zstd (default) or gzip (`--gz`)

The embedded UPXZ container is **codec-agnostic**: a single byte in the
embedded magic records which codec compressed the payload, so one `upxz` binary
handles both.

| codec | magic byte | flag      | level range            |
| ----- | ---------- | --------- | ---------------------- |
| zstd  | `0`        | _(none)_  | 1..=19 (default 19)    |
| gzip  | `1`        | `--gz`    | 1..=9 (default 9)      |

```bash
# pack with gzip instead of zstd (embedded codec byte = 1)
upxz --gz notes.txt              # -> notes.txt.upxz

# zstd is still the default and is fully backward-compatible
upxz notes.txt                   # -> notes.txt.upxz (embedded codec byte = 0, same as v0.2)
```

Every read path (`-d`/`-l`/`-t`) dispatches on the embedded codec byte, so you
do not need to tell `upxz` which codec a container used — it reads it from the
embedded magic. The gzip backend is `flate2` with the pure-Rust `miniz_oxide`
backend (no C `libz`); zstd is unchanged.

**macOS SFX caveat**: the macOS SFX loader is `no_std` + `zstd-sys` FFI for
size (~84 KB, under the 1/5-of-upxz gate). It cannot carry a gzip decoder, so
`upxz --gz` on macOS (or `upxz -c --gz`) refuses with a clear message. There
is no `upxz`-side workaround for gzip on macOS — the SFX runs the embedded
binary itself (no runner path), and the loader is zstd-only. Drop `--gz` on
macOS. The Linux SFX stub and the Windows winstub both support gzip.

## Why zstd-first (no xz / liblzma)

`upxz` does not depend on `xz2` / `liblzma`. zstd is the default codec so the
build stays a single binary with a permissive license story (Apache-2.0 project
+ BSD-licensed zstd bindings) and no LGPL entanglement. gzip is offered as an
opt-in (`--gz`) via the pure-Rust `miniz_oxide` backend for tooling that
already speaks gzip; it never adds a C dependency. See [`DESIGN.md`](DESIGN.md)
for the full rationale.

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
[ stub ELF ][ .upxz container (magic+namelen+name+compressed payload) ][ u64 stub_size BE ]
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

### Windows — temp-file SFX (`CreateProcessW`)

Windows has no portable in-memory exec (no `memfd_create`/`fexecve` analogue
that runs an arbitrary PE buffer without a hand-rolled loader). The Windows
SFX therefore mirrors the macOS disk-drop trade-off, but with a **single**
stub segment — the same shape as the Linux SFX:

```
[ stub PE ][ .upxz container ][ u64 stub_size BE ]
```

The stub (`winstub/` crate) resolves its own path via `GetModuleFileNameW`,
reads the trailing 8 bytes to recover `stub_size`, slices out the `.upxz`
container, decompresses it (zstd **or** gzip — the Windows stub is a normal
`std` binary with no size gate, unlike the macOS `no_std` loader), writes the
restored PE to `%TEMP%\upxz-<pid>-<tag>-<stem>.exe`, and `CreateProcessW`s it
with `argv[0]` set to the stored original name and the remaining `argv`
forwarded verbatim. The temp file is removed after the child exits.

```powershell
# build an SFX (Windows)
upxz -c C:\tools\myapp.exe -o .\myapp.sfx.exe
.\myapp.sfx.exe --flag value    # forwards --flag value to myapp.exe
echo $LASTEXITCODE              # myapp's exit code
```

**No ad-hoc code-signing** is required on Windows (unlike macOS AMFI, Windows
does not kill a local exec for lacking a signature). Windows Defender /
SmartScreen may prompt on the first run of an unknown `.exe` — that is host-
level behaviour for any newly-materialised binary and is not something upxz
can or should bypass.

**In-memory exec (NT section) is documented but not compiled.** Windows does
expose a way to start a process directly from a memory section
(`NtCreateSection` + `NtMapViewOfSection` + `NtCreateProcessEx`), which would
avoid the temp file entirely. It is intentionally **not implemented** here:
`windows-sys` exposes `NtCreateSection`/`NtMapViewOfSection` but not
`NtCreateProcessEx`, the resulting process has no initial thread / PEB / stack
(a hand-rolled loader is required), and it is the technique AV products most
aggressively flag. See mneme `docs/upxz/windows.md` for the analysis and the
PoC plan. Revisit when there is a Windows host to iterate on.

This feature is `#[cfg(target_os = "linux")]` / `#[cfg(target_os = "macos")]`
/ `#[cfg(target_os = "windows")]`. On any other target `upxz -c` refuses with
a clear message; the cross-platform runner path (`upxz foo.upxz`) still works.

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
