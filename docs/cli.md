---
layout: default
title: CLI
---

# CLI

upxz is a flat CLI (no subcommands). The mode is determined by what you
pass in:

- a **plain file** → packed into a runnable self-extractor
- an **already-packed** file (bare `.upxz` container or SFX) → **refused** with
  a clear hint to run the SFX directly or use `-d`

| invocation | action |
| --- | --- |
| `upxz <FILE>` | **pack** → `<FILE>.upxz` (self-extractor: stub + container + trailer, chmod +x) |
| `./<FILE>.upxz` | **run** → decompress + exec the original (that IS the run; propagates exit code) |
| `upxz <FILE>.upxz` | **refused** — already packed; run the `.upxz` directly or use `-d` |
| `upxz -d <FILE>.upxz` | **unpack** → restore the original (executable bit restored for executables) |
| `upxz -l <FILE>.upxz` | **list** → codec / sizes / original name (read-only) |
| `upxz -t <FILE>.upxz` | **test** → verify magic + round-trip decompress (read-only) |
| `upxz -c <orig> -o <packed>` | **pack** → self-extractor at an explicit output path |
| `upxz --bin <inner> <a.tar.zst> [-- args...]` | **bin run** → run one entry from a `.tar.zst` without extracting |

## Examples

```bash
# pack a file (zstd, default level 19) — produces a self-extractor (chmod +x)
upxz notes.txt                   # -> notes.txt.upxz

# run the self-extractor directly (that IS the run — upxz has no run mode)
./notes.txt.upxz
./notes.txt.upxz -- --flag value # args after -- forwarded verbatim to the inner program

# inspect / verify / restore (read-mostly) — all locate the embedded container
upxz -l notes.txt.upxz           # list: codec / sizes / original name
upxz -t notes.txt.upxz           # test: magic + round-trip decompress
upxz -d notes.txt.upxz           # unpack: restore the original bytes;
                                 #         chmod +x when the original was an executable
upxz -d notes.txt.upxz -f        # overwrite an existing notes.txt

# pick a compression level
upxz --fast notes.txt            # zstd level 1 — lowest CPU, hot loops
upxz -z 9 notes.txt              # zstd level 9  (any 1..=19)
upxz --gz notes.txt              # gzip instead of zstd (codec id 1 in the embedded magic)

# build a self-extractor at an explicit output path
upxz -c myapp -o myapp.sfx && ./myapp.sfx --flag value

# run one entry from a .tar.zst without extracting
upxz --bin bin/myapp app.tar.zst -- --flag value
```

## Compression

A 2-tier preset plus an explicit level — no `--best`, never `-22`
(a documented zstd trap: same size as `-19`, ~2× slower). `-z N` wins
over `--fast`:

| selection | flag | zstd level | when |
| --- | --- | --- | --- |
| default | _(none)_ | 19 | the common case (smallest) |
| fast | `--fast` | 1 | minimize CPU |
| explicit | `-z N` | N (1..=19) | pin a specific level |

`--gz` switches the codec to gzip (DEFLATE 1..=9, default 9); `-z N` then
sets the DEFLATE level. The codec byte is embedded in the container magic
(0 = zstd, 1 = gzip) — see [Format](/format/).

### macOS gzip SFX caveat

The macOS SFX loader is `no_std` + `zstd-sys` FFI for size (~84 KB) and
**cannot** carry a gzip decoder. `upxz --gz` on macOS (or
`upxz -c --gz`) refuses with a clear message. There is no `upxz`-side
workaround for gzip on macOS — drop `--gz`. The Linux SFX stub and the
Windows winstub both support gzip.

## Trailing args

For the SFX (run directly):

```bash
./packed -- --flag value   # args after -- are forwarded verbatim to the inner program
```

`argv[0]` of the inner process is the stored original basename.
