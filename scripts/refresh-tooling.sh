#!/usr/bin/env bash
# Print the current upstream packaging tool releases with their sha256, so
# packaging/tools.lock can be refreshed deliberately.
#
#   scripts/refresh-tooling.sh          # report only
#   scripts/refresh-tooling.sh --write  # rewrite packaging/tools.lock
#
# Nothing here is automatic: a new upstream tool is a change to code that
# ends up inside every image we ship, so it wants a human reading the diff.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

latest_tag() { # latest_tag OWNER/REPO
	curl -fsSL "https://api.github.com/repos/$1/releases" |
		python3 -c 'import json,sys; rs=[r for r in json.load(sys.stdin) if not r["prerelease"]]; print(rs[0]["tag_name"] if rs else "")'
}

AT_TAG="$(latest_tag AppImage/appimagetool)"
RT_TAG="$(latest_tag AppImage/type2-runtime)"
[ -n "$AT_TAG" ] && [ -n "$RT_TAG" ] || { echo "error: could not read upstream release tags" >&2; exit 1; }

AT_URL="https://github.com/AppImage/appimagetool/releases/download/$AT_TAG/appimagetool-x86_64.AppImage"
RT_URL="https://github.com/AppImage/type2-runtime/releases/download/$RT_TAG/runtime-x86_64"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL --retry 3 -o "$tmp/at" "$AT_URL"
curl -fsSL --retry 3 -o "$tmp/rt" "$RT_URL"
AT_SHA="$(sha256sum "$tmp/at" | cut -d' ' -f1)"
RT_SHA="$(sha256sum "$tmp/rt" | cut -d' ' -f1)"

current() { sed -n "s/^$1=//p" packaging/tools.lock; }

echo "appimagetool   current $(current appimagetool_version) -> available $AT_TAG"
echo "               sha256   $AT_SHA"
echo "type2-runtime  current $(current runtime_version) -> available $RT_TAG"
echo "               sha256   $RT_SHA"

if [ "${1:-}" = '--write' ]; then
	cat >packaging/tools.lock <<EOF
# Pinned packaging tooling, verified by sha256 in packaging/build-appimage.sh.
# Immutability matters here: these bytes go inside every published image, so
# a silent upstream change must fail the build rather than ship unreviewed
# code to users. Refresh on purpose with scripts/refresh-tooling.sh.
appimagetool_version=$AT_TAG
appimagetool_url=$AT_URL
appimagetool_sha256=$AT_SHA
runtime_version=$RT_TAG
runtime_url=$RT_URL
runtime_sha256=$RT_SHA
EOF
	echo
	echo "packaging/tools.lock rewritten - review and commit"
fi
