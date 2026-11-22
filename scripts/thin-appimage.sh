#!/usr/bin/env bash
# Repacks a Tauri AppImage so it runs on the host's WebKitGTK, GTK and GStreamer instead of
# the copies linuxdeploy bundles from the build machine. The bundled Ubuntu 22.04 stack froze
# the window (WebKitWebProcess at 100% CPU) and forced the Adwaita theme on newer distros.
# Hosts need webkit2gtk-4.1 installed, as Tauri's dev build does.
#
# Usage: scripts/thin-appimage.sh <file.AppImage>   (replaced in place; re-sign it afterwards)
# APPIMAGETOOL points at appimagetool when it is not on PATH.
set -euo pipefail

app=$(realpath "$1")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$work"
"$app" --appimage-extract >/dev/null
root="$work/squashfs-root"
test -x "$root/usr/bin/chimaera" || { echo "thin-appimage: usr/bin/chimaera missing in $app" >&2; exit 1; }

# Keep the app's resources (usr/lib/Chimaera) and its own libraries; drop the system stack.
find "$root/usr/lib" -mindepth 1 -maxdepth 1 ! -name Chimaera ! -name 'libhydra_*' -exec rm -rf {} +
rm -rf "$root/apprun-hooks" "$root/AppRun.wrapped"

cat > "$root/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$here/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$here/usr/bin/chimaera" "$@"
EOF
chmod +x "$root/AppRun"

# Every remaining ELF must resolve against the host (here: the build machine) without the bundle.
missing=$(find "$root/usr/bin" "$root/usr/lib" -type f -exec sh -c \
  'file -b "$1" | grep -q "^ELF" && LD_LIBRARY_PATH="$2" ldd "$1" | grep "not found" | sed "s|^|$1: |"' _ {} "$root/usr/lib" \; || true)
[ -z "$missing" ] || { echo "thin-appimage: unresolved libraries:" >&2; echo "$missing" >&2; exit 1; }

# --appimage-extract leaves 700 directories, which appimagetool refuses to pack.
chmod -R a+rX "$root"
ARCH=x86_64 "${APPIMAGETOOL:-appimagetool}" --no-appstream "$root" "$work/out.AppImage" >/dev/null
mv "$work/out.AppImage" "$app"
chmod +x "$app"
echo "thin-appimage: $(du -h "$app" | cut -f1) $app"
