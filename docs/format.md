---
layout: default
title: Format
---

# Format

upxz ships **two** on-disk forms of the same container:

1. A **bare UPXZ container** (the original v0.1–v0.3 form, still readable
   by `-d` / `-l` / `-t` for backward compatibility).
2. A **self-extracting** form (the default since v0.4.0) — a platform
   stub/loader prepended to the container, with a short trailer.

## Bare UPXZ container

```
+------------------+----------------------+--------------------------+
| magic (8 bytes)   | name-len (4 bytes BE)| original file name (UTF-8)|
+------------------+----------------------+--------------------------+
+--------------------------------------------------------------------+
| compressed payload (zstd or gzip, per the codec id in the magic)  |
+--------------------------------------------------------------------+
```

### Magic layout (8 bytes)

| offset | bytes | meaning |
| --- | --- | --- |
| 0..3 | `"UPXZ"` | ASCII tag |
| 4 | `0x01` | format version |
| 5 | codec id | `0` = zstd, `1` = gzip |
| 6..7 | `0x00 0x00` | reserved (must be zero) |

The reserved bytes must be zero — a future format that reuses them will
bump the version byte.

The stored file name is flat (no separators, no `..`). The `-d` step
re-validates it before writing, so a tampered container cannot escape
the current directory.

## Self-extractor (SFX) layout

The default pack since v0.4.0 embeds the container inside a
**self-extracting executable** that you run directly:

| platform | layout |
| --- | --- |
| Linux | `[ stub ELF ][ .upxz container ][ trailer: u64 stub_size BE ]` (8 bytes) |
| macOS | `[ loader Mach-O ][ .upxz container ][ trailer: UPXZEND1 + loader_len u32 BE + app_len u32 BE ]` (16 bytes) |
| Windows | `[ stub PE ][ .upxz container ][ trailer: u64 stub_size BE ]` (8 bytes) |

The stub/loader is a tiny no_std binary (`stub/` on Linux/Windows, `loader/`
on macOS, both compiled by `build.rs` for the target) that reads the
trailer, slices out the embedded container, decompresses it, and execs
the restored original. On macOS, the loader also re-signs the restored
copy ad-hoc (`codesign --sign - --force`) because macOS AMFI SIGKILLs
(exit 137) a copied+executed Mach-O whose signature no longer matches;
the Linux stub and Windows winstub have no signing requirement.

Running the SFX uses **no temp file on Linux** (memfd + `fexecve`) and a
**temp file on macOS/Windows** (`/tmp/upxz-app-<pid>` on macOS,
`%TEMP%\upxz-<pid>-<tag>-<stem>.exe` on Windows) that is removed after
the child exits.

## Read paths locate the container in both forms

`-d` / `-l` / `-t` operate on either form. The reader calls
`format::classify()` to classify the input as one of:

- `Plain` — not a packed artifact, refuse to re-pack
- `Packed { offset, len }` — the embedded UPXZ container is exactly
  `buf[offset..offset + len]` (offset 0 for a bare container;
  `stub_size` for Linux/Windows SFX; `loader_len` for macOS SFX)

This keeps v0.1–v0.3 bare containers readable (the container is at
offset 0) while the new self-extractors are read at the right offset.
The read paths are unchanged from the user's perspective:

```bash
upxz -d hello.txt.upxz          # works on bare container and SFX
upxz -l hello.txt.upxz          # works on both
upxz -t hello.txt.upxz          # works on both
```

## Codec id

The single codec byte in the embedded magic selects the payload codec
(0 = zstd, 1 = gzip). One `upxz` binary handles both — the read paths
dispatch on the embedded byte, not on a flag at the CLI. The gzip
backend is `flate2` with the pure-Rust `miniz_oxide` backend (no C
`libz`); zstd is unchanged.

```bash
upxz --gz notes.txt              # embedded codec byte = 1
upxz notes.txt                   # embedded codec byte = 0 (default, same as v0.1–v0.3)
```
