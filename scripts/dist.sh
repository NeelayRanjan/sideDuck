#!/bin/sh
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
./scripts/cargo.sh build --release --locked -p sideduck-app
bundle="$repo_dir/dist/sideDuck.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/sideduck-app "$bundle/Contents/MacOS/sideDuck"
cp config/prototype.json "$bundle/Contents/Resources/prototype.json"
cp config/input.json "$bundle/Contents/Resources/input.json"
cp crates/sideduck-brush/assets/brushes.json "$bundle/Contents/Resources/brushes.json"
cp -R crates/sideduck-brush/assets/presets "$bundle/Contents/Resources/"
./scripts/cargo.sh run --release --locked -p sideduck-app --example render_icon -- assets/icon.svg target/AppIcon.iconset
iconutil -c icns target/AppIcon.iconset -o "$bundle/Contents/Resources/AppIcon.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>sideDuck</string>
<key>CFBundleIdentifier</key><string>app.sideduck.editor</string>
<key>CFBundleName</key><string>sideDuck</string>
<key>CFBundleDisplayName</key><string>sideDuck</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundleDocumentTypes</key><array><dict>
<key>CFBundleTypeName</key><string>Painting document</string>
<key>CFBundleTypeExtensions</key><array><string>pond</string></array>
<key>CFBundleTypeRole</key><string>Editor</string>
<key>LSTypeIsPackage</key><true/>
</dict></array>
<key>NSHighResolutionCapable</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
printf 'Application bundle: %s\n' "$bundle"
