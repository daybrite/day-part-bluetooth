#!/bin/bash
set -euo pipefail
fixture_dir="$(cd "$(dirname "$0")" && pwd)"
fixture_sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
fixture_tools="$(find "$fixture_sdk/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -1)"
fixture_jar="$(find "$fixture_sdk/platforms" -name android.jar | sort -V | tail -1)"
fixture_serial="${1:?Usage: run.sh ANDROID_SERIAL}"
fixture_build="$(mktemp -d "${TMPDIR:-/tmp}/day-ble-android.XXXXXX")"
trap 'rm -rf "$fixture_build"' EXIT
mkdir -p "$fixture_build/compiled" "$fixture_build/gen" "$fixture_build/classes" "$fixture_build/dex"
"$fixture_tools/aapt2" compile --dir "$fixture_dir/res" -o "$fixture_build/compiled"
"$fixture_tools/aapt2" link -o "$fixture_build/fixture.apk" -I "$fixture_jar" --manifest "$fixture_dir/AndroidManifest.xml" --java "$fixture_build/gen" "$fixture_build/compiled/"*.flat
find "$fixture_dir/src" "$fixture_build/gen" -name '*.java' -print0 | xargs -0 javac --release 17 -classpath "$fixture_jar" -d "$fixture_build/classes"
find "$fixture_build/classes" -name '*.class' -print0 | xargs -0 "$fixture_tools/d8" --min-api 31 --lib "$fixture_jar" --output "$fixture_build/dex"
jar uf "$fixture_build/fixture.apk" -C "$fixture_build/dex" classes.dex
"$fixture_tools/zipalign" -f 4 "$fixture_build/fixture.apk" "$fixture_build/aligned.apk"
keytool -genkeypair -keystore "$fixture_build/test.jks" -storepass android -keypass android -alias fixture -keyalg RSA -validity 2 -dname 'CN=Disposable Day BLE fixture' >/dev/null 2>&1
"$fixture_tools/apksigner" sign --ks "$fixture_build/test.jks" --ks-pass pass:android --out "$fixture_build/signed.apk" "$fixture_build/aligned.apk"
# Each build uses a fresh disposable signing key; remove only this dedicated fixture package.
"$fixture_sdk/platform-tools/adb" -s "$fixture_serial" uninstall dev.daybrite.blefixture >/dev/null 2>&1 || true
"$fixture_sdk/platform-tools/adb" -s "$fixture_serial" install "$fixture_build/signed.apk"
for fixture_permission in BLUETOOTH_CONNECT BLUETOOTH_ADVERTISE; do
    "$fixture_sdk/platform-tools/adb" -s "$fixture_serial" shell pm grant dev.daybrite.blefixture "android.permission.$fixture_permission"
done
"$fixture_sdk/platform-tools/adb" -s "$fixture_serial" shell input keyevent KEYCODE_WAKEUP
"$fixture_sdk/platform-tools/adb" -s "$fixture_serial" shell am start -n dev.daybrite.blefixture/.MainActivity
