#!/bin/sh
# upxz-boot — macOS three-segment SFX boot script.
#
# This file is prepended (verbatim) to a packed file by `upxz -c` on macOS.
# Layout of the resulting packed file:
#
#   [ this boot script ][ upxz-loader bytes ][ .upxz app container ][ trailer ]
#
# The trailer (last 24 bytes) is:
#   b"UPXZEND1"  (8 bytes magic)
#   boot_len     (u32 big-endian)   <- length of THIS script segment
#   loader_len   (u32 big-endian)
#   app_len      (u32 big-endian)
#
# When the packed file is executed, the kernel reads this shebang and runs
# `/bin/sh` on the whole file. sh ignores the trailing binary bytes (they are
# past the final newline of valid script text), so the packed file does not
# need to be codesigned. See mneme `docs/upxz/codesign-decision.md` for why
# this works (verified: a sh-packed file with embedded Mach-O bytes still
# execs unsigned on macOS, because the kernel treats the file as a script,
# not a Mach-O).
#
# Boot's job: extract the upxz-loader segment to a cache dir (reused across
# invocations and across different packed files, since the loader is generic),
# sign it ad-hoc once, then exec it with the packed file path as argv[1] and
# all user args forwarded.

set -e  # any error in boot itself aborts before the loader runs.

# argv[0] is the path to this packed file as invoked. Use it to read self.
self=$0

# Read the trailer: last 20 bytes. We use tail(1) for the last 20 bytes, then
# parse the three u32 big-endian lengths.
#
# Byte layout of the trailer (20 bytes total):
#   0..8   = "UPXZEND1"
#   8..12  = boot_len   (u32 BE)
#   12..16 = loader_len (u32 BE)
#   16..20 = app_len    (u32 BE)
trailer_len=20

# Use a portable approach: read the file size, then dd the trailer region.
# `wc -c < "$self"` gives the size in bytes (portable).
size=$(wc -c < "$self" | tr -d ' ')
if [ -z "$size" ]; then
    echo "upxz-boot: cannot stat packed file $self" >&2
    exit 74
fi

# Extract trailer bytes directly from the file at offset (size - trailer_len).
# We do NOT capture binary into a shell variable: binary bytes can be mangled
# by `$()` (NUL truncation, encoding issues). Instead we read each field with
# `dd` from the file at the right offset, pipe straight to `od`, and assemble
# in shell arithmetic. This is fully binary-safe.
trailer_off=$((size - trailer_len))

# Verify the magic: read 8 bytes at trailer_off.
magic=$(dd if="$self" bs=1 skip="$trailer_off" count=8 2>/dev/null | od -An -c | tr -d ' \n')
# od -c prints characters; the magic is pure ASCII so the comparison is clean.
# We collapse runs of whitespace so "U P X Z E N D 1" -> "UPXZEND1".
magic=$(printf '%s' "$magic" | tr -d ' ')
if [ "$magic" != "UPXZEND1" ]; then
    echo "upxz-boot: bad trailer magic (not an upxz packed file)" >&2
    exit 65
fi

# Parse a big-endian u32 at a given absolute file offset. Reads 4 bytes with
# dd, pipes to od -tu1 (one byte per decimal token), assembles BE in $(( )).
# We must NOT let the bytes be interpreted as octal: force base-10 with
# `10#` prefix, because `$(( 0020 ))` is octal in POSIX shell arithmetic and
# would silently mis-parse zero-padded od output.
read_u32_be_at() {
    off=$1
    # Read 4 bytes; od -An -tu1 prints each byte as a decimal integer. We use
    # `read` to split on whitespace (handles od's variable padding) and force
    # base-10 to avoid octal interpretation of leading zeros.
    set -- $(dd if="$self" bs=1 skip="$off" count=4 2>/dev/null | od -An -tu1)
    b0=$((10#$1)); b1=$((10#$2)); b2=$((10#$3)); b3=$((10#$4))
    echo $(( (b0 << 24) | (b1 << 16) | (b2 << 8) | b3 ))
}

boot_len=$(read_u32_be_at $((trailer_off + 8)))
loader_len=$(read_u32_be_at $((trailer_off + 12)))
app_len=$(read_u32_be_at $((trailer_off + 16)))

# Sanity: lengths must be consistent with file size.
loader_start=$boot_len
loader_end=$((boot_len + loader_len))
app_start=$loader_end
app_end=$((loader_end + app_len))
trailer_start=$((size - trailer_len))
if [ "$app_end" -gt "$trailer_start" ] || [ "$boot_len" -lt 0 ] || [ "$loader_len" -le 0 ] || [ "$app_len" -le 0 ]; then
    echo "upxz-boot: trailer segment lengths are inconsistent with file size" >&2
    exit 65
fi

# Extract the upxz-loader segment to a cache dir. Reused across invocations:
# the loader is generic (it reads the packed file's own trailer for the app
# offset), so one cached copy serves every packed file produced by the same
# upxz version. We key the cache by the loader's length (a coarse version
# proxy) so a newer upxz that ships a different-sized loader does not reuse a
# stale cached copy.
cache_dir=${UPXZ_CACHE_DIR:-"$HOME/.cache/upxz"}
loader_cache="$cache_dir/upxz-loader-$loader_len"

mkdir -p "$cache_dir"

# NOTE: we deliberately do NOT garbage-collect stale `/tmp/upxz-app-*` temp
# files here. The loader names its temp `/tmp/upxz-app-<pid>` and cannot unlink
# it after execv (forking a watchdog from the ad-hoc-signed no_std loader
# triggers AMFI SIGKILL on the exec'd program). A naive `rm -f /tmp/upxz-app-*`
# sweep in boot would race with a CONCURRENT invocation whose loader is still
# mid-exec on its own temp file — deleting the in-use file and SIGKILLing the
# peer. The residual files are harmless: chmod 0500, owner-only, and bounded
# by pid reuse. Users who want them gone can `rm /tmp/upxz-app-*` manually or
# reboot (macOS clears /tmp on boot).

if [ ! -x "$loader_cache" ]; then
    # Extract the loader segment to a temp file in the same dir, then atomically
    # move into place. This avoids a partial write being visible to a concurrent
    # invocation.
    tmp_loader="$cache_dir/.upxz-loader.$$"
    dd if="$self" of="$tmp_loader" bs=1 skip="$loader_start" count="$loader_len" 2>/dev/null
    chmod 0500 "$tmp_loader"
    # Ad-hoc sign the extracted loader: macOS AMFI SIGKILLs (exit 137) an
    # unsigned Mach-O on exec. codesign is present on every macOS install.
    codesign --sign - --force -- "$tmp_loader" >/dev/null 2>&1 || true
    mv -f "$tmp_loader" "$loader_cache"
fi

# Exec the loader with: argv[0]=loader, argv[1]=packed path, argv[1..]=user args.
# `exec` replaces the shell process so the loader becomes the running process
# and inherits stdin/stdout/stderr/env unchanged. We forward "$@" verbatim,
# including args that start with `-` (the shell does not re-parse them).
exec "$loader_cache" "$self" "$@"
