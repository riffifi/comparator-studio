#!/bin/sh
set -eu

if [ "$(uname -s)" != Darwin ]; then
    echo "Build the macOS app bundle on a Mac." >&2
    exit 1
fi

cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR=target
cargo build --locked --release

version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
app="dist/Comparator Studio.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 target/release/comparator-studio "$app/Contents/MacOS/comparator-studio"
install -m 644 assets/comparator.icns "$app/Contents/Resources/comparator.icns"
install -m 644 assets/Geist-OFL.txt "$app/Contents/Resources/Geist-OFL.txt"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>comparator-studio</string>
    <key>CFBundleIdentifier</key><string>app.comparator.studio</string>
    <key>CFBundleName</key><string>Comparator Studio</string>
    <key>CFBundleDisplayName</key><string>Comparator Studio</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleIconFile</key><string>comparator.icns</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
printf 'Built %s\n' "$app"
