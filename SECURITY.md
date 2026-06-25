# Security Policy

## Scope

This document describes the security properties of `upxz` (a Rust CLI that
wraps a single file in a small magic-checked container and compresses it with
zstd).

## Threat model

`upxz` reads one input file and writes one output file on the local
filesystem. It executes no subcommands, performs no network access, and links
only to:

- the Rust standard library,
- `clap` (argument parsing),
- `anyhow` (error reporting), and
- `zstd` (libzstd FFI bindings, MIT-licensed).

## Container hardening

- The container starts with an 8-byte magic. `pack` refuses to wrap input that
  already carries the magic, preventing accidental double-packing.
- The original file name stored in the header is a **flat path component** —
  no separators, no `.` / `..`. `unpack` re-validates the stored name before
  writing, so a tampered container cannot write outside the current directory
  via a crafted header. Use `-o` to choose an explicit output path when
  unpacking untrusted containers.
- The declared name length is bounded (`MAX_NAME_LEN`), so a corrupted length
  field cannot cause a multi-gigabyte allocation before the payload is read.

## Reporting a vulnerability

Please open a private security advisory:
**<https://github.com/ljh-sh/upxz/security/advisories/new>**.

Do not open a public issue for suspected security problems.
