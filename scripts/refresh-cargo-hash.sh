#!/usr/bin/env bash
# Refresh nix/cargoHash.txt from a real build instead of inventing a hash.
#
# Nix pins the vendored crate graph by hash, so any Cargo.lock change makes
# the stored value stale. The failure message carries the correct hash, so:
# try the build, read the hash it reports, write it, build again to confirm.
#
#   scripts/refresh-cargo-hash.sh          # report only
#   scripts/refresh-cargo-hash.sh --write  # update nix/cargoHash.txt
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v nix >/dev/null || {
	echo "error: nix is required (flakes: https://nixos.org/download.html)" >&2
	exit 1
}

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

if nix build '.#openchatd' --no-link -L >"$log" 2>&1; then
	echo "nix/cargoHash.txt is current:"
	cat nix/cargoHash.txt
	exit 0
fi

# Fixed-output mismatches report the real value; anything else is a build
# failure we must not paper over.
got="$(sed -n 's/^[[:space:]]*\(got\|actual\):[[:space:]]*\(sha256-[A-Za-z0-9+/=]\{40,\}\).*/\2/p' "$log" | tail -1)"
if [ -z "$got" ]; then
	echo "the build failed for a reason unrelated to the hash - full output:" >&2
	cat "$log" >&2
	exit 1
fi

echo "stale: $(cat nix/cargoHash.txt)"
echo "current: $got"

if [ "${1:-}" = '--write' ]; then
	printf '%s\n' "$got" >nix/cargoHash.txt
	echo "nix/cargoHash.txt written - verifying with a second build"
	nix build '.#openchatd' --no-link -L
	echo "verified: nix build succeeds with the new hash"
else
	echo "(report only; rerun with --write)"
fi
