#!/bin/sh
# Builds the macOS downloads from a compiled program:
#   gridwatch-v<version>-<platform>.dmg     for people (open it, drag the app to Applications)
#   gridwatch-v<version>-<platform>.tar.gz  for the updater inside gridwatch
# usage: package.sh <program> <version> <platform> <output directory>
set -eu
program=$1
version=$2
platform=$3
out=$(cd "$4" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
name="gridwatch-v$version-$platform"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The application bundle: started by double click, it opens no Terminal window.
app="$work/$name/gridwatch.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$program" "$app/Contents/MacOS/gridwatch"
chmod 755 "$app/Contents/MacOS/gridwatch"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# The icon in the sizes macOS asks for, from the one PNG.
iconset="$work/gridwatch.iconset"
mkdir "$iconset"
for size in 16 32 128 256; do
    sips -z "$size" "$size" "$root/packaging/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$root/packaging/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
cp "$root/packaging/icon.png" "$iconset/icon_512x512.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/gridwatch.icns"

# Not signed by a developer account; an ad-hoc signature is what macOS needs to run it at all.
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

cp "$root/README.md" "$root/CHANGELOG.md" "$root/LICENSE" "$work/$name/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"

image="$work/image"
mkdir "$image"
cp -R "$app" "$image/"
ln -s /Applications "$image/Applications"
hdiutil create -volname "gridwatch $version" -srcfolder "$image" -ov -format UDZO "$out/$name.dmg" >/dev/null

echo "$out/$name.dmg"
echo "$out/$name.tar.gz"
