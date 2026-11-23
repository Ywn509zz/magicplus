#!/bin/sh
# Construit MagicPlus.app (macOS) à partir de la version release.
#
# Usage : packaging/macos/build-app.sh [dossier de sortie]
#         (par défaut : target/release/MagicPlus.app)
#         MAGICPLUS_UNIVERSAL=1 : exécutable universel (Intel + Apple Silicon), utilisé par GitHub ;
#         il faut alors `rustup target add x86_64-apple-darwin aarch64-apple-darwin`.
# Installer ensuite : glisser MagicPlus.app dans Applications, ou
#         ditto target/release/MagicPlus.app /Applications/MagicPlus.app
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/target/release}"
APP="$OUT/MagicPlus.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

cd "$ROOT"
if [ "${MAGICPLUS_UNIVERSAL:-0}" = "1" ]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
  cargo build --release --locked --target x86_64-apple-darwin
  cargo build --release --locked --target aarch64-apple-darwin
  mkdir -p target/universal
  lipo -create -output target/universal/magicplus \
    target/x86_64-apple-darwin/release/magicplus target/aarch64-apple-darwin/release/magicplus
  BINARY=target/universal/magicplus
else
  cargo build --release
  BINARY=target/release/magicplus
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/magicplus"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

# Icône : PNG 512 px -> AppIcon.icns
WORK="$(mktemp -d)"
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" packaging/macos/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  if [ "$double" -le 512 ]; then
    sips -z "$double" "$double" packaging/macos/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  fi
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$WORK"

# Signature locale (« ad hoc ») : macOS garde ainsi les autorisations accordées à l'app.
codesign --force --sign - "$APP"

echo "$APP"
