<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: MPL-2.0
-->

# day-part-bluetooth

Connect a Day app to Bluetooth Low Energy sensors, accessories, and other peripherals.
Scan for devices, discover their GATT services, read and write values, and receive notifications.

The same API supports scripted peripherals for automated tests. Install a simulation to test
application code with contrived replies and events, including disconnects and errors.

## Use

This package is not yet published. From a sibling app checkout:

```toml
[dependencies]
day-part-bluetooth = { path = "../day-part-bluetooth" }
```

Build with the Day CLI so the app includes native bridge code and permissions. The first release
requires Day's external bridge source support; see the [CI and release guide](docs/ci.md).

After scanning and selecting a device, read its battery characteristic:

```rust
use day_part_bluetooth as ble;

async fn battery(device: &str) -> Result<Vec<u8>, ble::Error> {
    ble::request_access().await?;
    ble::connect(device).await?;
    let reading = async {
        ble::services(device).await?;
        ble::read(device, "180f", "2a19").await
    }
    .await;
    let disconnected = ble::disconnect(device).await;
    let reading = reading?;
    disconnected?;
    Ok(reading)
}
```

The UUIDs identify the standard Battery Service and Battery Level characteristic. The device must
provide them. Discover services first when working with an unfamiliar peripheral.

## Platform support

| Platform | Native integration | Setup or limit |
| --- | --- | --- |
| iOS and macOS | CoreBluetooth | Localized permission description; Bluetooth entitlement for sandboxed macOS apps. |
| Android | Bluetooth GATT | Nearby devices permissions; location permission depends on the app's scanning declaration and OS version. |
| Linux | BlueZ | System D-Bus and a powered adapter. Known dual-mode Classic/LE devices are currently rejected. |
| Windows | WinRT | LE adapter; Bluetooth capability for packaged apps. Scan before connecting. |
| Web and HarmonyOS | Unavailable | Simulations work; native calls report unsupported. |

The API operates as a central connecting to peripherals. It does not expose peripheral
advertising, Classic Bluetooth profiles, or background reconnection.

## Try it

```sh
cargo run --example mock_peripheral
cargo test
cd demo
day launch -t macos-appkit --script dayscript/demo.yaml
```

The mock example and tests use synthetic data without radio access. Bluetooth Lab also supports a
[software peripheral](docs/bluetooth.md#demo-and-software-peripheral) for testing native GATT traffic
between physical devices.

## Documentation

- [Scanning, connections, permissions, and platform limits](docs/bluetooth.md)
- [Intercept Bluetooth calls in tests](docs/bluetooth.md#test-without-bluetooth-hardware)
- [CI and first release setup](docs/ci.md)
- [Run Bluetooth Lab](demo/README.md)

Generate the Rust API reference with `cargo doc --no-deps --open`.

[MPL-2.0 license](LICENSE)
