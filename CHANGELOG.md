# v0.2.0 — SFX (self-extracting binary)

Major: upxz now produces **self-extracting binaries** (`upxz -c <orig> <packed>` → `./packed` runs directly, self-contained, no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
<<<<<<< HEAD
- **macOS**: three-part SFX (`#!/bin/sh` boot + no_std upxz-loader + app) — packed body unsigned (sh), loader codesign verified.
=======
- **macOS**: two-segment SFX (`[upxz-loader][.upxz][trailer]`) — the loader Mach-O IS the packed file's header (codesigned); `./packed` execs the loader directly, which decompresses the app segment and execs the original. (`codesign --verify --strict` fails on the appended bytes, but AMFI accepts exec — the trade-off any appended-payload SFX makes on macOS.)
>>>>>>> 9097309 (fix: restore v0.2.0 version and CHANGELOG (revert accidental regression))
- `upxz-loader`: no_std + zstd-sys FFI, **84KB (1/17.9 of full upxz)** — passes <1/5 gate.
- 2-tier zstd (default 19 / --fast 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- Post-impl validation: ./packed runs (busybox echo / macli), 22+ tests.

<<<<<<< HEAD
Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part).
=======
Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS loader+app).
>>>>>>> 9097309 (fix: restore v0.2.0 version and CHANGELOG (revert accidental regression))
