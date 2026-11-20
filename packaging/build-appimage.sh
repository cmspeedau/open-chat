#!/usr/bin/env bash
# Build the open-chat AppImage.
#
# One script for both local runs and CI (see .github/workflows), so the
# published image is never built by a different recipe than the one you
# tested. Requirements: a built binary (default target/release/openchatd),
# mksquashfs (squashfs-tools), curl, sha256sum, objdump. Optional:
# desktop-file-validate (validates the .desktop when present) and zsyncmake
# (delta updates, enabled with --updateinformation).
#
# Tooling is pinned by checksum in packaging/tools.lock and cached under
# .cache/packaging, so upstream cannot silently change what we ship.
# Refresh with: packaging/tools.lock is updated by scripts/refresh-tooling.sh
#
# PORTABILITY NOTE: an AppImage borrows the *host* libc. Build the image you
# publish on the oldest distribution you intend to support; --max-glibc makes
# a too-new build fail loudly instead of shipping something that dies with
# "GLIBC_2.xx not found" on a user's machine.
set -euo pipefail

ROOT='' BIN='' VERSION='' COMP="${LDAI_COMP:-zstd}" OUT_DIR='dist' MAX_GLIBC='' UPDATE_INFORMATION="${UPDATE_INFORMATION:-}"

usage() {
	cat <<'USAGE'
usage: packaging/build-appimage.sh [options]
  --bin PATH             binary to package        [default target/release/openchatd]
  --version STRING       version for the filename  [default: from `openchatd --version`]
  --out DIR               output directory         [default: dist]
  --comp ALGO             squashfs compression     [default: zstd]
  --max-glibc VER         fail if the binary needs a newer GLIBC than VER (e.g. 2.35)
  --updateinformation STR zsync/update info embedded in the image
  -h, --help              this message
USAGE
}

while [ "$#" -gt 0 ]; do
	case "$1" in
		--bin) BIN="$2"; shift 2 ;;
		--version) VERSION="$2"; shift 2 ;;
		--out) OUT_DIR="$2"; shift 2 ;;
		--comp) COMP="$2"; shift 2 ;;
		--max-glibc) MAX_GLIBC="$2"; shift 2 ;;
		--updateinformation) UPDATE_INFORMATION="$2"; shift 2 ;;
		-h | --help) usage; exit 0 ;;
		*)
			echo "unknown argument: $1" >&2
			usage >&2
			exit 2
			;;
	esac
done

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${BIN:-target/release/openchatd}"
[ -x "$BIN" ] || {
	echo "error: $BIN is missing or not executable - run 'cargo build --release' first" >&2
	exit 1
}

# The binary itself is the version source of truth: what we package is what
# it reports, so a stale manifest edit cannot produce a mislabelled image.
VERSION="${VERSION:-$("$BIN" --version | awk '{print $2}')}"
[ -n "$VERSION" ] || { echo "error: could not determine a version from '$BIN --version'" >&2; exit 1; }

command -v mksquashfs >/dev/null || { echo "error: mksquashfs not found (install squashfs-tools)" >&2; exit 1; }
command -v curl >/dev/null || { echo "error: curl not found" >&2; exit 1; }
command -v objdump >/dev/null || { echo "error: objdump not found (binutils)" >&2; exit 1; }

# ------------------------------------------------------- portability floor ----
# Checked before anything is assembled: a binary that cannot meet the stated
# floor must fail early, not after an hour of packaging that produces an
# image nobody on an older distro can run.
FLOOR="$(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1)"
echo "glibc floor of the binary to package: $FLOOR"
if [ -n "$MAX_GLIBC" ]; then
	if [ "$(printf '%s\n%s\n' "$MAX_GLIBC" "$FLOOR" | sort -V | tail -1)" != "$MAX_GLIBC" ]; then
		echo "error: binary needs GLIBC_$FLOOR but the release floor is GLIBC_$MAX_GLIBC" >&2
		echo "       build on an older base (CI uses ubuntu-22.04)" >&2
		exit 1
	fi
	echo "portability gate: OK (<= $MAX_GLIBC)"
fi

echo "libs the target must still provide (never bundled by design):"
ldd "$BIN" | sed -n 's/^\s*\([^ ]*\)\s*=>\s\([^ ]*\).*/  \1/p' | head -12 || true

# ---------------------------------------------------------------- AppDir ----
APPDIR_ROOT="build/appimage"
AD="$APPDIR_ROOT/open-chat.AppDir"
rm -rf "$AD"
mkdir -p \
	"$AD/usr/bin" \
	"$AD/usr/share/applications" \
	"$AD/usr/share/metainfo" \
	"$AD/usr/share/icons/hicolor/256x256/apps" \
	"$AD/usr/share/icons/hicolor/512x512/apps" \
	"$AD/usr/share/doc/open-chat"

install -m 0755 "$BIN" "$AD/usr/bin/openchatd"
install -m 0644 packaging/open-chat.desktop "$AD/open-chat.desktop"
install -m 0644 packaging/open-chat.desktop "$AD/usr/share/applications/open-chat.desktop"
install -m 0755 packaging/AppRun "$AD/AppRun"
install -m 0644 packaging/icons/256x256/open-chat.png "$AD/usr/share/icons/hicolor/256x256/apps/open-chat.png"
install -m 0644 packaging/icons/512x512/open-chat.png "$AD/usr/share/icons/hicolor/512x512/apps/open-chat.png"
install -m 0644 packaging/icons/512x512/open-chat.png "$AD/open-chat.png"
install -m 0644 packaging/icons/256x256/open-chat.png "$AD/.DirIcon"
install -m 0644 packaging/open-chat.metainfo.xml "$AD/usr/share/metainfo/open-chat.metainfo.xml"
install -m 0644 crates/daemon/assets/NOTICE "$AD/usr/share/doc/open-chat/NOTICE"

# The image is extracted by anyone who has it: no secrets, and the modes we
# set here are what every user receives.
chmod -R go-w,a+rX "$AD"

if command -v desktop-file-validate >/dev/null 2>&1; then
	desktop-file-validate "$AD/open-chat.desktop"
	echo "desktop file: valid"
else
	echo "desktop file: NOT validated (desktop-file-validate not installed)" >&2
fi

if command -v appstreamcli >/dev/null 2>&1; then
	# appstreamcli exits 3 for both errors and warnings, so the report text
	# decides: any "errors:" is fatal, a warnings-only report is recorded and
	# passed. The one warning we carry is cid-desktopapp-is-not-rdns, which is
	# a Flathub convention; matching it would break the AppImage rule that the
	# desktop file's basename is the application name.
	report="$(appstreamcli validate "$AD/usr/share/metainfo/open-chat.metainfo.xml" 2>&1)" || true
	echo "$report" | grep -vE '^\s*$' | head -20
	if echo "$report" | grep -q 'errors:'; then
		echo "error: appstream metadata has real errors" >&2
		exit 1
	fi
	if echo "$report" | grep -q 'warnings:'; then
		echo "appstream metadata: no errors (warnings above are deliberate, see comment)"
	else
		echo "appstream metadata: clean"
	fi
else
	echo "appstream metadata: NOT validated (appstreamcli not installed)" >&2
fi

# ------------------------------------------------------------------ tools ----
CACHE=".cache/packaging"
mkdir -p "$CACHE"
TOOLS_URL="$(sed -n 's/^appimagetool_url=//p' packaging/tools.lock)"
TOOLS_SHA="$(sed -n 's/^appimagetool_sha256=//p' packaging/tools.lock)"
RUNTIME_URL="$(sed -n 's/^runtime_url=//p' packaging/tools.lock)"
RUNTIME_SHA="$(sed -n 's/^runtime_sha256=//p' packaging/tools.lock)"
TOOL="$CACHE/appimagetool-x86_64.AppImage"
RUNTIME="$CACHE/runtime-x86_64"

fetch_pinned() { # fetch_pinned URL SHA DEST
	url="$1" sha="$2" dest="$3"
	[ -f "$dest" ] || curl -fsSL --retry 3 -o "$dest" "$url"
	if ! echo "$sha  $dest" | sha256sum -c --status -; then
		echo "error: $dest does not match the pinned sha256 in packaging/tools.lock" >&2
		echo "       got     $(sha256sum "$dest" | cut -d' ' -f1)" >&2
		echo "       expected $sha" >&2
		echo "       if upstream really moved, refresh packaging/tools.lock deliberately" >&2
		exit 1
	fi
}

fetch_pinned "$TOOLS_URL" "$TOOLS_SHA" "$TOOL"
fetch_pinned "$RUNTIME_URL" "$RUNTIME_SHA" "$RUNTIME"
chmod +x "$TOOL"

# mksquashfs only prints option *categories* in -help, so the compressor is
# probed the honest way: try to build a throwaway squashfs with it.
probe="$(mktemp -d)"
mkdir -p "$probe/in"
echo probe >"$probe/in/f"
if ! mksquashfs "$probe/in" "$probe/out" -comp "$COMP" -noappend >/dev/null 2>&1; then
	echo "note: mksquashfs has no '$COMP' here, falling back to gzip" >&2
	COMP='gzip'
	if ! mksquashfs "$probe/in" "$probe/out2" -comp gzip -noappend >/dev/null 2>&1; then
		echo "error: mksquashfs cannot build any squashfs on this host" >&2
		rm -rf "$probe"
		exit 1
	fi
fi
rm -rf "$probe"

# ----------------------------------------------------------------- build ----
ARCH="${ARCH:-x86_64}"
OUT_DIR_ABS="$(mkdir -p "$OUT_DIR" && CDPATH='' cd -- "$OUT_DIR" && pwd)"
RESULT="$OUT_DIR_ABS/open-chat-$VERSION-$ARCH.AppImage"
rm -f "$RESULT" "$RESULT.zsync"

# appimagetool self-extracts when there is no /dev/fuse, and that moves its
# working directory: every path it receives must be absolute, and the
# destination is named explicitly so the filename is ours (name-version-arch)
# instead of inferred from the AppDir.
#
# -n skips *appimagetool's* AppStream check only: appstreamcli exits 3 for
# warnings and errors alike, so the tool aborts on a style note
# (cid-desktopapp-is-not-rdns). The metadata is not left unchecked - it is
# validated above with the severity parsed out of the report.
AD_ABS="$(CDPATH='' cd -- "$AD" && pwd)"
RUNTIME_ABS="$(CDPATH='' cd -- "$(dirname "$RUNTIME")" && pwd)/$(basename "$RUNTIME")"

export APPIMAGE_EXTRACT_AND_RUN=1 ARCH VERSION

tool_args=("$AD_ABS" "$RESULT" -n --comp "$COMP" --runtime-file "$RUNTIME_ABS")
[ -n "$UPDATE_INFORMATION" ] && tool_args+=(--updateinformation "$UPDATE_INFORMATION")

"$TOOL" "${tool_args[@]}"
[ -f "$RESULT" ] || { echo "error: appimagetool produced no $RESULT" >&2; exit 1; }

ZSYNC=''
[ -f "$RESULT.zsync" ] && ZSYNC="$RESULT.zsync"

# ------------------------------------------------------------ smoke test ----
echo "smoke test:"
APPIMAGE_EXTRACT_AND_RUN=1 "$RESULT" --version
[ -n "$ZSYNC" ] && echo "zsync delta manifest: $ZSYNC"

(
	cd "$OUT_DIR_ABS"
	sha256sum "$(basename "$RESULT")" >SHA256SUMS.txt
	[ -n "$ZSYNC" ] && sha256sum "$(basename "$ZSYNC")" >>SHA256SUMS.txt
	cat SHA256SUMS.txt
)

echo
echo "built: $RESULT"
