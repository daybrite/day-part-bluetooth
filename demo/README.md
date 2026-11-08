# Bluetooth Lab

Scan for nearby Bluetooth LE devices, inspect their services, and test a synthetic peripheral
with [day-part-bluetooth](../README.md).

From this directory, run a target supported by your host:

```sh
day launch -t ios-uikit --script dayscript/demo.yaml
day launch -t android-material --script dayscript/demo.yaml
day launch -t macos-appkit --script dayscript/demo.yaml
```

The portable walkthrough checks the controls without starting a scan. Use a physical device to
exercise native Bluetooth; simulators and hosted runners generally lack usable LE radios.

For a complete read, write, and notification test, start the
[software peripheral](../docs/bluetooth.md#demo-and-software-peripheral) on another device and
choose **Test software peripheral**. The test writes only to that fixture's private service.

See [permissions and platform setup](../docs/bluetooth.md#permissions) before scanning.
UI strings and Apple permission descriptions are localized in `resource/locales/`.
