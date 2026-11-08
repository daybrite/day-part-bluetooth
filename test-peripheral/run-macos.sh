#!/bin/bash
set -euo pipefail
fixture_dir="$(cd "$(dirname "$0")" && pwd)"
fixture_build="$(mktemp -d "${TMPDIR:-/tmp}/day-ble-fixture.XXXXXX")"
trap 'rm -rf "$fixture_build"' EXIT
cat > "$fixture_build/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.daybrite.bluetooth.fixture</string>
<key>CFBundleName</key><string>Day BLE test fixture</string>
<key>NSBluetoothAlwaysUsageDescription</key><string>Advertise a synthetic Bluetooth peripheral for local Day tests.</string>
</dict></plist>
PLIST
xcrun swiftc "$fixture_dir/Peripheral.swift" -o "$fixture_build/peripheral" \
  -framework CoreBluetooth -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker "$fixture_build/Info.plist"
"$fixture_build/peripheral"
