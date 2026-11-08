// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Bluetooth LE central operations. Subscribe before scanning or connecting to observe adapter,
//! connection and characteristic changes. Native identifiers are opaque and platform-specific.
//! Calls do not pair devices or write characteristics without an explicit request.

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
    pub services: Vec<String>,
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

/// Ask for Android scan/connect permissions from a UI action. Apple presents its prompt when
/// the central manager is first used; the app must declare its localized usage explanation.
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
    if native_supported() {
        Ok(())
    } else {
        Err(error(ErrorKind::Unsupported, "Bluetooth is unavailable"))
    }
}

/// Callbacks may run on a native delivery thread; use a Day Setter to update the UI.
fn native_watch(mut on_event: impl FnMut(Event) + Send + 'static) -> Result<Watch, Error> {
    Ok(Watch(WatchInner::Native(
        native::watch_native_stream(move |item| {
            let result = match item {
                day_bridge::Item::Value(value) => serde_json::from_str(&value)
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
/// Uses the global session. In simulation this never requests OS permissions.
pub async fn request_access() -> Result<(), Error> {
    Session::global().request_access().await
}
/// Keep the returned watch alive. Its backend is fixed when the watch is created.
pub fn watch(callback: impl FnMut(Event) + Send + 'static) -> Result<Watch, Error> {
    Session::global().watch(callback)
}
pub async fn start_scan(options: &ScanOptions) -> Result<(), Error> {
    Session::global().start_scan(options).await
}
pub async fn stop_scan() -> Result<(), Error> {
    Session::global().stop_scan().await
}
pub async fn connect(device: &str) -> Result<(), Error> {
    Session::global().connect(device).await
}
pub async fn disconnect(device: &str) -> Result<(), Error> {
    Session::global().disconnect(device).await
}
pub async fn services(device: &str) -> Result<Vec<Service>, Error> {
    Session::global().services(device).await
}
pub async fn read(device: &str, service: &str, characteristic: &str) -> Result<Vec<u8>, Error> {
    Session::global()
        .read(device, service, characteristic)
        .await
}
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
    for uuid in &options.services {
        validate_uuid(uuid)?;
    }
    command(json!({"op":"scan", "services": options.services, "duplicates":options.duplicates}))
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
    serde_json::from_value(value).map_err(|e| error(ErrorKind::Platform, e.to_string()))
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
    validate_uuid(service)?;
    validate_uuid(characteristic)?;
    request["op"] = json!(op);
    request["device"] = json!(device);
    request["service"] = json!(service);
    request["characteristic"] = json!(characteristic);
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

fn error(kind: ErrorKind, message: impl Into<String>) -> Error {
    Error {
        kind,
        message: message.into(),
    }
}
fn bridge_error(e: day_bridge::Error) -> Error {
    let kind = if matches!(e, day_bridge::Error::Unsupported) {
        ErrorKind::Unsupported
    } else {
        ErrorKind::Platform
    };
    error(kind, e.to_string())
}
fn validate_uuid(uuid: &str) -> Result<(), Error> {
    let valid = match uuid.len() {
        4 | 8 => uuid.bytes().all(|b| b.is_ascii_hexdigit()),
        36 => uuid.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(error(ErrorKind::InvalidArgument, "invalid Bluetooth UUID"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_validation_accepts_assigned_numbers_and_rejects_native_parser_traps() {
        for good in ["180F", "0000180f", "0000180f-0000-1000-8000-00805f9b34fb"] {
            assert!(validate_uuid(good).is_ok());
        }
        for bad in ["", "zzzz", "180f\n", "0000180f_0000-1000-8000-00805f9b34fb"] {
            assert!(validate_uuid(bad).is_err());
        }
    }
}
