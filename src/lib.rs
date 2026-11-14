// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Bluetooth LE central operations. Subscribe before scanning or connecting to observe adapter,
//! connection and characteristic changes. Native identifiers are opaque and platform-specific.
//! Every UUID the native API accepts or returns is canonical: lowercase, 128-bit, with hyphens
//! ([`canonical_uuid`]). Calls do not pair devices or write characteristics without an explicit
//! request.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod bluez;
#[cfg(any(
    all(target_os = "linux", not(target_env = "ohos")),
    target_os = "windows"
))]
mod desktop;
mod native;
mod session;
pub mod simulation;
pub use session::{Session, SimulationGuard};
#[cfg(target_os = "windows")]
mod winrt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Unsupported,
    PermissionDenied,
    PoweredOff,
    Busy,
    Disconnected,
    NotFound,
    InvalidArgument,
    Timeout,
    Platform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for Error {}

/// The adapter as the platform reports it. On Apple platforms the state stays `Unknown` until
/// [`request_access`] or the first operation creates the central manager, which is also what
/// shows the system's Bluetooth prompt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterState {
    Unknown,
    Resetting,
    Unsupported,
    Unauthorized,
    PoweredOff,
    PoweredOn,
}

/// An advertisement does not establish a device's identity or trustworthiness.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Advertisement {
    pub device: String,
    pub name: Option<String>,
    pub rssi: i32,
    /// Canonical service UUIDs ([`canonical_uuid`]).
    pub services: Vec<String>,
    /// The company identifier, little endian, followed by the payload, as the Bluetooth
    /// specification lays it out.
    pub manufacturer_data: Vec<u8>,
    pub connectable: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    State {
        state: AdapterState,
    },
    Advertisement {
        advertisement: Advertisement,
    },
    Connected {
        device: String,
    },
    Disconnected {
        device: String,
    },
    /// A notification or indication from a subscribed characteristic. The answer to an explicit
    /// [`read`] is returned by that call and does not arrive here.
    Value {
        device: String,
        service: String,
        characteristic: String,
        value: Vec<u8>,
    },
    Error {
        error: Error,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Characteristic {
    pub uuid: String,
    pub read: bool,
    pub write: bool,
    pub write_without_response: bool,
    pub notify: bool,
    pub indicate: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Service {
    pub uuid: String,
    pub characteristics: Vec<Characteristic>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ScanOptions {
    /// An empty list scans all advertised services. Use filters to reduce radio and UI work.
    pub services: Vec<String>,
    pub duplicates: bool,
}

/// Dropping a watch stops event delivery. Stop scanning and disconnect explicitly when finished.
pub struct Watch(WatchInner);
enum WatchInner {
    Native(u64),
    Simulated {
        _subscription: simulation::Subscription,
    },
}
impl Drop for Watch {
    fn drop(&mut self) {
        if let WatchInner::Native(token) = &self.0 {
            let _ = native::unwatch(*token as i64);
            native::watch_native_stop(*token);
        }
    }
}

fn native_supported() -> bool {
    cfg!(any(
        target_os = "ios",
        target_os = "macos",
        all(target_os = "linux", not(target_env = "ohos")),
        target_os = "windows",
        target_os = "android"
    ))
}

/// The canonical spelling of a Bluetooth UUID: lowercase, 128-bit, hyphenated. Accepts 16-bit
/// (`180F`), 32-bit (`0000180F`) and 128-bit forms in either case; assigned numbers expand on
/// the Bluetooth base UUID. Every string the native API returns is already in this form, so
/// comparing against `canonical_uuid("180F")?` works on every platform. Simulations see the
/// UUIDs an app passed, unchanged.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidArgument`] when `uuid` is not a 16-bit, 32-bit or 128-bit
/// hexadecimal UUID.
pub fn canonical_uuid(uuid: &str) -> Result<String, Error> {
    let lower = uuid.to_ascii_lowercase();
    let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
    let canonical = match lower.len() {
        4 if hex(&lower) => format!("0000{lower}-0000-1000-8000-00805f9b34fb"),
        8 if hex(&lower) => format!("{lower}-0000-1000-8000-00805f9b34fb"),
        36 if lower.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }) =>
        {
            lower
        }
        _ => return Err(error(ErrorKind::InvalidArgument, "invalid Bluetooth UUID")),
    };
    Ok(canonical)
}

/// Android requests the scan/connect permissions (and location, unless the app declares
/// `neverForLocation`); every platform then reports the adapter's state. Apple creates the
/// central manager here, which shows the system prompt the first time, and answers once
/// CoreBluetooth reports a state.
async fn native_request_access() -> Result<(), Error> {
    #[cfg(target_os = "android")]
    {
        use day_part_permissions::{Permission, Status, android};
        let api = command(json!({"op":"api_level"}))
            .await?
            .as_u64()
            .unwrap_or(0);
        let mut permissions = if api >= 31 {
            vec![android::BLUETOOTH_SCAN, android::BLUETOOTH_CONNECT]
        } else {
            Vec::new()
        };
        if command(json!({"op":"scan_needs_location"}))
            .await?
            .as_bool()
            .unwrap_or(true)
        {
            permissions.push(Permission::Location);
        }
        let statuses = day_part_permissions::request_many_future(&permissions).await;
        if statuses.iter().any(|s| *s != Status::Granted) {
            return Err(error(
                ErrorKind::PermissionDenied,
                "Bluetooth permission was not granted",
            ));
        }
    }
    if !native_supported() {
        return Err(error(ErrorKind::Unsupported, "Bluetooth is unavailable"));
    }
    let state: AdapterState = serde_json::from_value(command(json!({"op":"access"})).await?)
        .map_err(|e| error(ErrorKind::Platform, e.to_string()))?;
    match state {
        AdapterState::PoweredOn => Ok(()),
        AdapterState::Unauthorized => Err(error(
            ErrorKind::PermissionDenied,
            "Bluetooth access was not granted",
        )),
        AdapterState::PoweredOff => Err(error(ErrorKind::PoweredOff, "Bluetooth is off")),
        AdapterState::Unsupported => Err(error(
            ErrorKind::Unsupported,
            "Bluetooth LE is unavailable on this device",
        )),
        AdapterState::Unknown | AdapterState::Resetting => {
            Err(error(ErrorKind::Busy, "Bluetooth adapter is not ready"))
        }
    }
}

/// Callbacks may run on a native delivery thread; use a Day Setter to update the UI.
fn native_watch(mut on_event: impl FnMut(Event) + Send + 'static) -> Result<Watch, Error> {
    Ok(Watch(WatchInner::Native(
        native::watch_native_stream(move |item| {
            let result = match item {
                day_bridge::Item::Value(value) => serde_json::from_str(&value)
                    .map(normalize_event)
                    .map_err(|e| error(ErrorKind::Platform, e.to_string())),
                day_bridge::Item::Failed(e) => Err(bridge_error(e)),
                day_bridge::Item::End => return,
            };
            on_event(result.unwrap_or_else(|error| Event::Error { error }));
        })
        .map_err(bridge_error)?,
    )))
}

/// Native availability, or true while the global session has an installed simulation.
pub fn supported() -> bool {
    Session::global().supported()
}
/// Ask for Bluetooth access from a UI action and wait until the adapter's state is known. `Ok`
/// means the adapter is powered on; a refused prompt is `PermissionDenied` and a radio that is
/// off is `PoweredOff`. Uses the global session; in simulation this never requests OS
/// permissions.
///
/// # Errors
///
/// Returns [`ErrorKind::PermissionDenied`] for a refused prompt, [`ErrorKind::PoweredOff`] when
/// the radio is off, and [`ErrorKind::Unsupported`] where the platform has no Bluetooth LE.
pub async fn request_access() -> Result<(), Error> {
    Session::global().request_access().await
}
/// Keep the returned watch alive. Its backend is fixed when the watch is created. Creating a
/// watch shows no prompt; on Apple platforms its first state event is `Unknown` until
/// [`request_access`] runs.
///
/// # Errors
///
/// Returns [`ErrorKind::Unsupported`] where the platform has no Bluetooth LE, and the
/// platform's error when the watch cannot start.
pub fn watch(callback: impl FnMut(Event) + Send + 'static) -> Result<Watch, Error> {
    Session::global().watch(callback)
}
/// Start scanning for advertisements, which arrive as [`Event::Advertisement`]. A new scan
/// replaces the one in progress.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidArgument`] for a malformed service UUID, and the adapter's error
/// otherwise, such as [`ErrorKind::PoweredOff`] or [`ErrorKind::PermissionDenied`].
pub async fn start_scan(options: &ScanOptions) -> Result<(), Error> {
    Session::global().start_scan(options).await
}
/// Stop the scan in progress.
///
/// # Errors
///
/// Returns the adapter's error when the scan cannot be stopped.
pub async fn stop_scan() -> Result<(), Error> {
    Session::global().stop_scan().await
}
/// Connect and wait for the link. Android also negotiates the largest MTU the peripheral allows
/// before this resolves, so [`max_write_length`] is known afterwards.
///
/// # Errors
///
/// Returns the platform's error when the link cannot be established, such as
/// [`ErrorKind::NotFound`], [`ErrorKind::Timeout`] or [`ErrorKind::PoweredOff`].
pub async fn connect(device: &str) -> Result<(), Error> {
    Session::global().connect(device).await
}
/// Close the connection to a peripheral.
///
/// # Errors
///
/// Returns the platform's error when the request cannot be delivered.
pub async fn disconnect(device: &str) -> Result<(), Error> {
    Session::global().disconnect(device).await
}
/// Discover every service and characteristic of a connected peripheral.
///
/// # Errors
///
/// Returns [`ErrorKind::Disconnected`] when the peripheral is not connected, and the
/// platform's error when discovery fails.
pub async fn services(device: &str) -> Result<Vec<Service>, Error> {
    Session::global().services(device).await
}
/// The largest payload [`write`] accepts for this connected peripheral, in bytes, for the given
/// write type. Longer payloads fail with `InvalidArgument` on every platform; the package does
/// not fragment messages.
///
/// # Errors
///
/// Returns [`ErrorKind::Disconnected`] when the peripheral is not connected, and the
/// platform's error otherwise.
pub async fn max_write_length(device: &str, with_response: bool) -> Result<usize, Error> {
    Session::global()
        .max_write_length(device, with_response)
        .await
}
/// Read a characteristic's value. Dropping the future while the read is in flight closes the
/// connection, so a late answer cannot complete a different request; reconnect before continuing.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidArgument`] for a malformed UUID, [`ErrorKind::NotFound`] for an
/// unknown characteristic, [`ErrorKind::Disconnected`] when the link is down, and the
/// platform's error when the read fails.
pub async fn read(device: &str, service: &str, characteristic: &str) -> Result<Vec<u8>, Error> {
    Session::global()
        .read(device, service, characteristic)
        .await
}
/// Write a value. `with_response` waits for the peripheral's acknowledgement and requires the
/// `write` property; without it the `write_without_response` property is required and the call
/// resolves once the radio accepted the bytes. Payloads above [`max_write_length`] are rejected
/// with `InvalidArgument`. Cancellation behaves as for [`read`].
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidArgument`] for a malformed UUID or an oversized payload,
/// [`ErrorKind::NotFound`] for an unknown characteristic, [`ErrorKind::Disconnected`] when the
/// link is down, and the platform's error when the write fails.
pub async fn write(
    device: &str,
    service: &str,
    characteristic: &str,
    value: &[u8],
    with_response: bool,
) -> Result<(), Error> {
    Session::global()
        .write(device, service, characteristic, value, with_response)
        .await
}
/// Subscribe to notifications or indications; later values arrive as [`Event::Value`].
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidArgument`] for a malformed UUID, [`ErrorKind::NotFound`] for an
/// unknown characteristic, [`ErrorKind::Unsupported`] when it can neither notify nor indicate,
/// and the platform's error otherwise.
pub async fn set_notify(
    device: &str,
    service: &str,
    characteristic: &str,
    enabled: bool,
) -> Result<(), Error> {
    Session::global()
        .set_notify(device, service, characteristic, enabled)
        .await
}

async fn native_start_scan(options: &ScanOptions) -> Result<(), Error> {
    let services = options
        .services
        .iter()
        .map(|uuid| canonical_uuid(uuid))
        .collect::<Result<Vec<_>, _>>()?;
    command(json!({"op":"scan", "services": services, "duplicates":options.duplicates}))
        .await
        .map(|_| ())
}
async fn native_stop_scan() -> Result<(), Error> {
    command(json!({"op":"stop_scan"})).await.map(|_| ())
}
async fn native_connect(device: &str) -> Result<(), Error> {
    command(json!({"op":"connect", "device":device}))
        .await
        .map(|_| ())
}
async fn native_disconnect(device: &str) -> Result<(), Error> {
    command(json!({"op":"disconnect", "device":device}))
        .await
        .map(|_| ())
}
async fn native_services(device: &str) -> Result<Vec<Service>, Error> {
    let value = command(json!({"op":"services", "device":device})).await?;
    let services: Vec<Service> =
        serde_json::from_value(value).map_err(|e| error(ErrorKind::Platform, e.to_string()))?;
    Ok(services
        .into_iter()
        .map(|mut service| {
            service.uuid = canonical_or_verbatim(service.uuid);
            for characteristic in &mut service.characteristics {
                characteristic.uuid =
                    canonical_or_verbatim(std::mem::take(&mut characteristic.uuid));
            }
            service
        })
        .collect())
}
async fn native_max_write_length(device: &str, with_response: bool) -> Result<usize, Error> {
    let value =
        command(json!({"op":"max_write", "device":device, "with_response":with_response})).await?;
    value
        .as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| error(ErrorKind::Platform, "missing write length"))
}
async fn native_read(device: &str, service: &str, characteristic: &str) -> Result<Vec<u8>, Error> {
    let value = characteristic_command("read", device, service, characteristic, json!({})).await?;
    serde_json::from_value(value).map_err(|e| error(ErrorKind::Platform, e.to_string()))
}
async fn native_write(
    device: &str,
    service: &str,
    characteristic: &str,
    value: &[u8],
    with_response: bool,
) -> Result<(), Error> {
    characteristic_command(
        "write",
        device,
        service,
        characteristic,
        json!({"value":value, "with_response":with_response}),
    )
    .await
    .map(|_| ())
}
async fn native_set_notify(
    device: &str,
    service: &str,
    characteristic: &str,
    enabled: bool,
) -> Result<(), Error> {
    characteristic_command(
        "notify",
        device,
        service,
        characteristic,
        json!({"enabled":enabled}),
    )
    .await
    .map(|_| ())
}

async fn characteristic_command(
    op: &str,
    device: &str,
    service: &str,
    characteristic: &str,
    mut request: Value,
) -> Result<Value, Error> {
    request["op"] = json!(op);
    request["device"] = json!(device);
    request["service"] = json!(canonical_uuid(service)?);
    request["characteristic"] = json!(canonical_uuid(characteristic)?);
    command(request).await
}

async fn command(request: Value) -> Result<Value, Error> {
    let pending = native::request_native_future(&request.to_string());
    let token = pending.token();
    let response = pending
        .on_cancel(move || {
            let _ = native::cancel(token as i64);
        })
        .await
        .map_err(bridge_error)?;
    let mut response: Value =
        serde_json::from_str(&response).map_err(|e| error(ErrorKind::Platform, e.to_string()))?;
    if let Some(failure) = response.get_mut("error") {
        return Err(serde_json::from_value(failure.take())
            .map_err(|e| error(ErrorKind::Platform, e.to_string()))?);
    }
    response
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| error(ErrorKind::Platform, "missing result"))
}

/// The platform's spelling of a UUID in canonical form, or the original string when the platform
/// produced something the parser does not recognize (reported as-is rather than dropped).
fn canonical_or_verbatim(uuid: String) -> String {
    canonical_uuid(&uuid).unwrap_or(uuid)
}

fn normalize_event(event: Event) -> Event {
    match event {
        Event::Advertisement { mut advertisement } => {
            advertisement.services = advertisement
                .services
                .into_iter()
                .map(canonical_or_verbatim)
                .collect();
            Event::Advertisement { advertisement }
        }
        Event::Value {
            device,
            service,
            characteristic,
            value,
        } => Event::Value {
            device,
            service: canonical_or_verbatim(service),
            characteristic: canonical_or_verbatim(characteristic),
            value,
        },
        other => other,
    }
}

fn error(kind: ErrorKind, message: impl Into<String>) -> Error {
    Error {
        kind,
        message: message.into(),
    }
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "passed to `map_err`, which hands the error over by value"
)]
fn bridge_error(e: day_bridge::Error) -> Error {
    let kind = if matches!(e, day_bridge::Error::Unsupported) {
        ErrorKind::Unsupported
    } else {
        ErrorKind::Platform
    };
    error(kind, e.to_string())
}
/// Rejects malformed UUIDs before they reach a native parser; simulations keep the spelling.
fn validate_uuid(uuid: &str) -> Result<(), Error> {
    canonical_uuid(uuid).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATTERY: &str = "0000180f-0000-1000-8000-00805f9b34fb";

    #[test]
    fn canonical_uuid_expands_assigned_numbers_and_lowercases_long_forms() {
        assert_eq!(canonical_uuid("180F").unwrap(), BATTERY);
        assert_eq!(canonical_uuid("0000180f").unwrap(), BATTERY);
        assert_eq!(
            canonical_uuid("0000180F-0000-1000-8000-00805F9B34FB").unwrap(),
            BATTERY
        );
        assert_eq!(
            canonical_uuid("E55733E0-DA7B-4C45-AC2D-5D559A122F01").unwrap(),
            "e55733e0-da7b-4c45-ac2d-5d559a122f01"
        );
    }

    #[test]
    fn uuid_validation_rejects_native_parser_traps() {
        for bad in [
            "",
            "zzzz",
            "180f\n",
            "0000180f_0000-1000-8000-00805f9b34fb",
            "180",
        ] {
            assert_eq!(
                validate_uuid(bad).unwrap_err().kind,
                ErrorKind::InvalidArgument,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn apple_and_android_spellings_normalize_to_one_event() {
        let apple: Event = serde_json::from_str(
            r#"{"event":"value","device":"A","service":"180F","characteristic":"2A19","value":[99]}"#,
        )
        .unwrap();
        let android: Event = serde_json::from_str(
            r#"{"event":"value","device":"A","service":"0000180f-0000-1000-8000-00805f9b34fb","characteristic":"00002a19-0000-1000-8000-00805f9b34fb","value":[99]}"#,
        )
        .unwrap();
        assert_eq!(normalize_event(apple), normalize_event(android));
        let advertisement: Event = serde_json::from_str(
            r#"{"event":"advertisement","advertisement":{"device":"A","name":null,"rssi":-50,"services":["FE9F"],"manufacturer_data":[],"connectable":true}}"#,
        )
        .unwrap();
        let Event::Advertisement { advertisement } = normalize_event(advertisement) else {
            panic!("not an advertisement");
        };
        assert_eq!(
            advertisement.services,
            vec!["0000fe9f-0000-1000-8000-00805f9b34fb".to_string()]
        );
    }

    #[test]
    fn unparseable_platform_uuids_pass_through_instead_of_disappearing() {
        assert_eq!(canonical_or_verbatim("odd".into()), "odd");
    }
}
