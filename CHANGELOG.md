# v0.2.0 — SFX (self-extracting binary)

Major: `upxz -c <orig> -o <packed>` produces a **self-extracting binary**; `./packed` runs directly, self-contained (no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
- **macOS**: two-segment SFX (loader Mach-O + app) — loader codesign, `./packed` execs loader directly (no boot; packed body is the loader).
- **upxz-loader**: `no_std` + `zstd-sys` FFI, **84KB (1/17.9 of full upxz)** — passes `<1/5` gate + `<100KB` target.
- 2-tier zstd (default 19 / `--fast` 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- 29 tests pass.

Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part A, superseded), #7 (macOS two-segment B).

# Unreleased

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
  `winstub/src/main.rs` and mneme `docs/upxz/windows.md`. The temp-file path
  is the supported Windows mechanism.
- **Status**: code complete + **cross-compile-verified to
  `x86_64-pc-windows-gnu`** (`cargo build --release --target
  x86_64-pc-windows-gnu` succeeds, producing a valid PE32+ `upxz.exe`).
  **Awaiting real-Windows runtime validation** (develop host is macOS).
  Linux/macOS SFX branches are untouched (`#[cfg]`-isolated); 32 existing
  tests still pass.
- **Build fix**: the recursive `cargo build -p upxz-winstub` in `build.rs`
  deadlocked in release cross-compile on the workspace package lock; resolved
  by isolating the recursive build's `CARGO_TARGET_DIR` under `OUT_DIR` and
  using `TARGET` (not `HOST`) as the winstub build triple.
- Refs: mneme `docs/upxz/windows.md`; PR #10 (`feat/sfx-windows`).

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
- 6 new gzip integration tests + 1 backward-compat guard; 47 tests pass total.
- Refs: mneme `story/feature/260626.upxz-tech/` §codec-agnostic.

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
- 4 new integration tests (`--bin` run/argv/exit, `./`-prefix match, missing
  entry) + 1 regression test for `run` trailing args.
