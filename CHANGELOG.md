# v0.2.0 — SFX (self-extracting binary)

Major: `upxz -c <orig> <packed>` produces a **self-extracting binary**; `./packed` runs directly, self-contained (no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
- **macOS**: two-segment SFX (loader Mach-O + app) — loader codesign, `./packed` execs loader directly (no boot; packed body is the loader).
- **upxz-loader**: `no_std` + `zstd-sys` FFI, **84KB (1/17.9 of full upxz)** — passes `<1/5` gate + `<100KB` target.
- 2-tier zstd (default 19 / `--fast` 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- 29 tests pass.

Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part A, superseded), #7 (macOS two-segment B).
