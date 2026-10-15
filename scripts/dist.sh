#!/bin/sh
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
./scripts/cargo.sh build --release --locked -p sideduck-app
bundle="$repo_dir/dist/sideDuck.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/sideduck-app "$bundle/Contents/MacOS/sideDuck"
cp config/prototype.json "$bundle/Contents/Resources/prototype.json"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>sideDuck</string>
<key>CFBundleIdentifier</key><string>app.sideduck.prototype</string>
<key>CFBundleName</key><string>sideDuck</string>
<key>CFBundleDisplayName</key><string>sideDuck</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
printf 'Prototype bundle: %s\n' "$bundle"
