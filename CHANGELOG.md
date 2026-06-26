# v0.2.0 — SFX (self-extracting binary)

Major: upxz now produces **self-extracting binaries** (`upxz -c <orig> <packed>` → `./packed` runs directly, self-contained, no external upxz needed).

- **Linux/Unix**: SFX stub via `memfd_create` + `fexecve` — pure in-memory exec, no temp file.
- **macOS**: three-part SFX (`#!/bin/sh` boot + no_std upxz-loader + app) — packed body unsigned (sh), loader codesign verified.
- `upxz-loader`: no_std + zstd-sys FFI, **84KB (1/17.9 of full upxz)** — passes <1/5 gate.
- 2-tier zstd (default 19 / --fast 1) + `-z N` (1..19); flat upx-style CLI; argv passthrough.
- Post-impl validation: ./packed runs (busybox echo / macli), 22+ tests.

Refs: mneme#41, upxz PR#5 (Linux memfd), #6 (macOS three-part).
