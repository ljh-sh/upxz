# Unreleased

- **pack now produces a self-extractor (upx-style default).** `upxz <FILE>`
  emits a runnable `<FILE>.upxz` (chmod +x) instead of a plain container; you
  run it directly (`./<FILE>.upxz`). The previous `upxz <packed>` runner
  mode is gone — feeding upxz an already-packed file (bare container or
  self-extractor) is now **refused** with a clear hint to run the SFX directly
  or restore the original with `-d`. This makes `upxz ./zhhz` produce a
  `./zhhz.upxz` that actually runs on macOS (the long-standing "can't be used"
  gap when the SFX wasn't the default). The new `classify()` in `format.rs`
  detects both bare containers and SFXes via their trailers, so the read
  paths (`-d`/`-l`/`-t`) and the refuse check share one source of truth and
  keep working on v0.1–v0.3 plain containers (backward compatible).

- **`-d` restores the executable bit.** When the original was an executable
  (ELF / Mach-O / PE / `#!`-shebang), `upxz -d` now chmod's the restored file
  to `0755` so it runs without an extra `chmod +x`. Non-executables keep the
  default non-exec mode. (upxz's container stores no mode bits, so this is a
  heuristic on the restored bytes' magic rather than an exact restoration —
  the alternative is a format change, which is out of scope here.)

- **Cross-arch release binaries**: the release matrix now ships **aarch64 Linux
  and x86_64 macOS** in addition to x86_64 Linux, aarch64 macOS, and x86_64
  Windows. `aarch64-unknown-linux-gnu` builds on a native ARM runner;
  `x86_64-apple-darwin` is cross-compiled on the arm64 macOS runner (no Intel
  mac runner remains).
- **`build.rs` honors `--target` for the SFX companion crates**: `build_linux_stub`
  switched `HOST` → `TARGET`, and `build_macos_pieces` now builds the loader with
  `--target`. Previously the stub/loader built for the host arch even when upxz
  was cross-compiled, so an x86_64-apple-darwin upxz embedded an **arm64**
  loader — the packed file's Mach-O header was the wrong arch. Native builds are
  unchanged (TARGET == HOST there). `build_windows_stub` already used `TARGET`.
- **macOS loader `fstat` → `lseek` (x86_64 SFX runtime fix)**: the no_std loader
  sized the packed file with `fstat` + a hand-rolled `struct stat`. On x86_64
  macOS the raw `fstat` symbol a Rust `extern` links is the **legacy 32-bit-inode
  variant** (`fstat`, not `fstat$INODE64` the C headers alias to); it writes a
  different, smaller struct, so `st_size` read as 0 and every x86_64 SFX failed
  at runtime with "packed file too small to contain a trailer". arm64 has no
  legacy symbol, which is why this was latent until cross-arch built a real
  x86_64 loader. Fixed by sizing via `lseek(fd, 0, SEEK_END)` — no struct-stat
  ABI dependency. Verified end-to-end (x86_64 SFX runs under Rosetta 2; arm64
  native unaffected; 45 tests green).
- **Release CI**: every build now passes `--target` explicitly and copies the
  artifact from `target/<triple>/release/`; a per-target **SFX smoke test**
  (pack `/bin/echo`, run it, assert output) guards the loader path — a green
  `cargo build` did not catch the fstat regression.

# v0.3.0 — three-platform SFX + codec-agnostic + `--bin` archive run

The first release with **self-extracting binaries on all three platforms**, a **codec-agnostic container** (zstd default, gzip via `--gz`), and **AppImage-style archive run** (`--bin`). Plus CI stability and a working crates.io publish path. **45 tests pass.**

- **Test coverage 32 → 45** (+13 regression guards): read-op independence
  (`-l`/`-t`/`-d` work without `-c`), CLI validation (`-c` w/o `-o`, `-o` w/o
  `-c`, no-args, missing-file), `-z` range (0/20 rejected), empty-file
  round-trip, payload-integrity (`-t` catches a corrupted payload byte via
  zstd's default content checksum), SFX runtime (stdin inheritance,
  stdout/stderr separation, truncated-trailer clean failure), and gzip-SFX
  rejected on macOS. PR #15.
- **Build fix — Linux `cargo build --release` deadlock** (#16):
  `build_linux_stub` recursed into `cargo build -p upxz-stub` against the same
  workspace `target/` the parent release build was locking → the child blocked
  on the target-dir lock and build.rs never returned (63-min CI hang). Fixed by
  isolating the recursive build's `CARGO_TARGET_DIR` under `OUT_DIR`, mirroring
  the macOS/Windows branches that already did this.
- **crates.io publish** (#11, PR #17): `build.rs` now detects whether the SFX
  companion-crate sources are present. From a git checkout / `cargo install
  --git` the stubs compile (full SFX). From a stripped crates.io tarball (cargo
  drops nested-package subtrees) it emits empty placeholders + clear
  `cargo:warning`s — `cargo publish` succeeds and `cargo install upxz` gives a
  working runner/packer; `upxz -c` points to `--git`/releases.
- **Integrity contract documented**: `-t` / unpack / run are true integrity
  checks — zstd's default content checksum (xxhash32) is verified by libzstd on
  every decode path, including the no_std macOS loader. See mneme
  `docs/upxz/integrity-check.md`.

Feature detail:

- **Windows SFX (`upxz -c` on Windows)**: a third platform for the
  self-extracting-binary feature. The Windows SFX layout mirrors the Linux
  stub (`[stub][.upxz][trailer: u64 stub_size BE]`); the `upxz-winstub` crate
  resolves its own path via `GetModuleFileNameW`, decompresses the `.upxz`
  payload (zstd **and** gzip — no size gate, unlike the macOS `no_std`
  loader), writes the restored PE to `%TEMP%`, and `CreateProcessW`s it with
  argv forwarded verbatim. **No ad-hoc code-signing** is required on Windows
  (unlike macOS AMFI). The in-memory NT-section route
  (`NtCreateSection`/`NtCreateProcessEx`) is **documented but not compiled** —
  `windows-sys` lacks `NtCreateProcessEx`, the resulting process has no
  initial thread/PEB, and it is the technique AV most aggressively flags; see
  `winstub/src/main.rs` and mneme `docs/upxz/sfx-stub-windows.md`. The
  temp-file path is the supported Windows mechanism.
- **Status**: code complete + **cross-compile-verified to
  `x86_64-pc-windows-gnu`** (`cargo build --release --target
  x86_64-pc-windows-gnu` succeeds, producing a valid PE32+ `upxz.exe`).
  **Awaiting real-Windows runtime validation** (develop host is macOS).
  Linux/macOS SFX branches are untouched (`#[cfg]`-isolated).
- Refs: mneme `docs/upxz/sfx-stub-windows.md`, `docs/upxz/windows.md`; PR #10.

- **Codec-agnostic container (`--gz`)**: the magic byte at offset 5 now carries
  a codec id — `0` = zstd (default, fully backward-compatible), `1` = gzip. The
  runner / unpacker / list / test paths all dispatch on this byte, so one upxz
  binary handles both codecs. `--gz` on pack selects gzip; `-z N` clamps to the
  DEFLATE range 1..=9 (default 9). Without `--gz`, pack writes zstd (codec 0),
  identical to v0.2 — every existing `.upxz` still runs unchanged.
- **Backends**: gzip via `flate2` with the pure-Rust `miniz_oxide` backend (no
  C `libz`, statically linked). zstd unchanged.
- **Linux SFX stub** supports both codecs. **macOS SFX loader** stays
  **zstd-only** for size (no_std + zstd-sys FFI, ~84 KB, < 1/5 of upxz); gzip on
  macOS goes through the cross-platform `upxz run` runner path.
- Refs: mneme `story/feature/260626.upxz-tech/` §codec-agnostic; PR #9.

- **`upxz --bin <inner-path> <archive.tar.zst> [-- args...]`**: run a single
  designated binary directly out of a `.tar.zst` archive **without extracting
  the whole archive** (AppImage-style). Streams zstd → tar, materializes only
  the matched entry (Linux: `memfd_create` + `fexecve`, in-memory only; macOS:
  temp file + ad-hoc codesign), and execs it with argv forwarded verbatim and
  exit code propagated. Other entries are read and discarded, never written to
  disk.
- **CLI fix**: the SFX output is now `-o`/`--out <packed>` instead of a second
  positional. This fixes a latent bug where `upxz foo.upxz -- -a -b` (runner
  trailing args) was swallowed by the SFX-output positional slot. `run` and
  `--bin` trailing args now work reliably.
- New deps: `tar = "0.4"`; `libc = "0.2"` (Linux target only).
- Refs: PR #8.

# v0.2.0 — SFX (self-extracting binary)

Major: `upxz -c <orig> -o <packed>` produces a **self-extracting binary**; `./packed` runs directly, self-contained (no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
- **macOS**: two-segment SFX (loader Mach-O + app) — loader codesign, `./packed` execs loader directly (no boot; packed body is the loader).
- **upxz-loader**: `no_std` + `zstd-sys` FFI, **84KB (1/17.9 of full upxz)** — passes `<1/5` gate + `<100KB` target.
- 2-tier zstd (default 19 / `--fast` 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- 29 tests pass.

Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part A, superseded), #7 (macOS two-segment B).
