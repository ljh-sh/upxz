# v0.2.0 — SFX (self-extracting binary)

Major: `upxz -c <orig> -o <packed>` produces a **self-extracting binary**; `./packed` runs directly, self-contained (no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
- **macOS**: two-segment SFX (loader Mach-O + app) — loader codesign, `./packed` execs loader directly (no boot; packed body is the loader).
- **upxz-loader**: `no_std` + `zstd-sys` FFI, **84KB (1/17.9 of full upxz)** — passes `<1/5` gate + `<100KB` target.
- 2-tier zstd (default 19 / `--fast` 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- 29 tests pass.

Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part A, superseded), #7 (macOS two-segment B).

# Unreleased

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
