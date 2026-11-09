<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

## Unreleased

- New: `canonical_uuid`. Every UUID the API accepts or returns is lowercase, 128-bit and
  hyphenated on every platform; 16-bit and 32-bit assigned numbers expand on the Bluetooth base
  UUID. Service, characteristic, advertisement and value-event UUIDs are normalized in Rust.
- New: `max_write_length(device, with_response)`. Android negotiates the largest MTU during
  `connect` and both arms reject longer payloads with `InvalidArgument` instead of failing at the
  GATT layer.
- Changed: `request_access` waits for the adapter's state on Apple platforms and reports
  `PermissionDenied`, `PoweredOff` or `Unsupported` from it. Creating a watch no longer creates
  the central manager, so the system's Bluetooth prompt appears at the user's action rather than
  at launch, and operations issued before the state is known are held instead of failing.
- Changed: `Event::Value` carries notifications and indications only; the answer to `read` is
  returned by that call.
- Changed: Android reports the cached device name when an advertisement carries none, as Apple
  does, and registers its adapter-state receiver privately.
- Changed: the demo lists nearby devices in a native list and inspects the selected one.

## 0.1.0 (2026-11-09)

- First release: CoreBluetooth, Android GATT, BlueZ and WinRT centrals behind one API, a
  simulation layer for tests, and the Bluetooth Lab demo with its software peripheral.
