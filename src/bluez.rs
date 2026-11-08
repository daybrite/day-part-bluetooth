// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! BlueZ central over the system D-Bus. One worker owns the connection and serializes calls;
//! callbacks and completions use Day's bridge, without an async runtime dependency.
use crate::desktop::Shared;
use crate::{AdapterState, Advertisement, Characteristic, Error, ErrorKind, Event, Service, error};
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::{Connection, stdintf::org_freedesktop_dbus::ObjectManager};
use dbus::message::MatchRule;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const SERVICE: &str = "org.bluez.GattService1";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";
const TIMEOUT: Duration = Duration::from_secs(15);
type Interfaces = HashMap<String, PropMap>;
type Objects = HashMap<dbus::Path<'static>, Interfaces>;

enum Signal {
    Added(dbus::Path<'static>, Interfaces),
    Removed(dbus::Path<'static>, Vec<String>),
    Changed(String, String, PropMap, Vec<String>),
}
pub(crate) struct Bluez {
    connection: Connection,
    signals: mpsc::Receiver<Signal>,
    shared: Arc<Shared>,
    objects: Objects,
    adapter: String,
    scanning: bool,
    filters: Vec<String>,
    duplicates: bool,
    seen: HashSet<String>,
    connected: HashSet<String>,
    notifications: HashMap<String, (String, String, String)>,
}
impl Bluez {
    pub(crate) fn new(shared: Arc<Shared>) -> Result<Self, Error> {
        let connection = Connection::new_system().map_err(bus_error)?;
        let (tx, signals) = mpsc::channel();
        let added = tx.clone();
        connection
            .add_match(
                MatchRule::new_signal("org.freedesktop.DBus.ObjectManager", "InterfacesAdded")
                    .with_sender("org.bluez"),
                move |(path, interfaces), _, _| {
                    let _ = added.send(Signal::Added(path, interfaces));
                    true
                },
            )
            .map_err(bus_error)?;
        let removed = tx.clone();
        connection
            .add_match(
                MatchRule::new_signal("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved")
                    .with_sender("org.bluez"),
                move |(path, interfaces), _, _| {
                    let _ = removed.send(Signal::Removed(path, interfaces));
                    true
                },
            )
            .map_err(bus_error)?;
        connection
            .add_match(
                MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
                    .with_sender("org.bluez"),
                move |(interface, properties, invalidated), _, message| {
                    if let Some(path) = message.path() {
                        let _ = tx.send(Signal::Changed(
                            path.to_string(),
                            interface,
                            properties,
                            invalidated,
                        ));
                    }
                    true
                },
            )
            .map_err(bus_error)?;
        let objects: Objects = connection
            .with_proxy("org.bluez", "/", TIMEOUT)
            .get_managed_objects()
            .map_err(bus_error)?;
        let mut adapters: Vec<_> = objects
            .iter()
            .filter_map(|(path, interfaces)| {
                interfaces
                    .get(ADAPTER)
                    .map(|props| (path.to_string(), boolean(props, "Powered")))
            })
            .collect();
        adapters.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let adapter = adapters
            .first()
            .map(|a| a.0.clone())
            .ok_or_else(|| error(ErrorKind::Unsupported, "no BlueZ Bluetooth adapter"))?;
        let result = Self {
            connection,
            signals,
            shared,
            objects,
            adapter,
            scanning: false,
            filters: Vec::new(),
            duplicates: false,
            seen: HashSet::new(),
            connected: HashSet::new(),
            notifications: HashMap::new(),
        };
        result.adapter_state();
        Ok(result)
    }
    pub(crate) fn adapter_state(&self) {
        let state = match self.props(&self.adapter, ADAPTER) {
            Some(props) if boolean(props, "Powered") => AdapterState::PoweredOn,
            Some(_) => AdapterState::PoweredOff,
            None => AdapterState::Unsupported,
        };
        self.shared.emit(Event::State { state });
    }
    fn props(&self, path: &str, interface: &str) -> Option<&PropMap> {
        self.objects
            .iter()
            .find(|(p, _)| &***p == path)?
            .1
            .get(interface)
    }
    fn refresh(&mut self) -> Result<(), Error> {
        self.objects = self
            .connection
            .with_proxy("org.bluez", "/", TIMEOUT)
            .get_managed_objects()
            .map_err(bus_error)?;
        Ok(())
    }
    fn method(&self, path: &str, interface: &str, method: &str) -> Result<(), Error> {
        let timeout = if method == "Connect" {
            Duration::from_secs(45)
        } else {
            TIMEOUT
        };
        self.connection
            .with_proxy("org.bluez", path, timeout)
            .method_call(interface, method, ())
            .map_err(bus_error)
    }
    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        for _ in 0..256 {
            if !self.connection.process(Duration::ZERO).map_err(bus_error)? {
                break;
            }
        }
        while let Ok(signal) = self.signals.try_recv() {
            match signal {
                Signal::Added(path, interfaces) => {
                    let device = interfaces.contains_key(DEVICE);
                    let key = path.to_string();
                    self.objects.entry(path).or_default().extend(interfaces);
                    if device {
                        self.advertise(&key);
                    }
                }
                Signal::Removed(path, interfaces) => {
                    let key = path.to_string();
                    if interfaces.iter().any(|i| i == DEVICE) {
                        self.disconnected(&key);
                    }
                    if let Some(object) = self.objects.get_mut(&path) {
                        for interface in interfaces {
                            object.remove(&interface);
                        }
                    }
                    if key == self.adapter {
                        self.adapter_state();
                        self.scanning = false;
                        self.lost();
                    }
                }
                Signal::Changed(path, interface, properties, invalidated) => {
                    let powered = interface == ADAPTER && properties.contains_key("Powered");
                    let disconnected = interface == DEVICE
                        && properties.contains_key("Connected")
                        && !boolean(&properties, "Connected");
                    let advertised = interface == DEVICE
                        && ["RSSI", "UUIDs", "ManufacturerData", "Name", "ServiceData"]
                            .iter()
                            .any(|k| properties.contains_key(*k));
                    let value = if interface == CHARACTERISTIC {
                        bytes(&properties, "Value")
                    } else {
                        None
                    };
                    let key =
                        dbus::Path::new(path.clone()).map_err(|e| error(ErrorKind::Platform, e))?;
                    let object = self
                        .objects
                        .entry(key)
                        .or_default()
                        .entry(interface.clone())
                        .or_default();
                    object.extend(properties);
                    for key in invalidated {
                        object.remove(&key);
                    }
                    if path == self.adapter && powered {
                        self.adapter_state();
                        if !self
                            .props(&path, ADAPTER)
                            .is_some_and(|p| boolean(p, "Powered"))
                        {
                            self.scanning = false;
                            self.lost();
                        }
                    }
                    if disconnected {
                        self.disconnected(&path);
                    }
                    if advertised {
                        self.advertise(&path);
                    }
                    if let (Some(value), Some((device, service, characteristic))) =
                        (value, self.notifications.get(&path))
                    {
                        self.shared.emit(Event::Value {
                            device: device.clone(),
                            service: service.clone(),
                            characteristic: characteristic.clone(),
                            value,
                        });
                    }
                }
            }
        }
        Ok(())
    }
    fn disconnected(&mut self, device: &str) {
        self.notifications
            .retain(|_, (owner, _, _)| owner != device);
        if self.connected.remove(device) {
            self.shared.emit(Event::Disconnected {
                device: device.into(),
            });
        }
    }
    fn advertise(&mut self, path: &str) {
        if !self.scanning || (!self.duplicates && self.seen.contains(path)) {
            return;
        }
        let Some(props) = self.props(path, DEVICE) else {
            return;
        };
        if text(props, "Adapter").as_deref() != Some(&self.adapter) {
            return;
        }
        let services = strings(props, "UUIDs");
        if !self.filters.is_empty()
            && !services
                .iter()
                .any(|s| self.filters.contains(&s.to_lowercase()))
        {
            return;
        }
        let advertisement = Advertisement {
            device: path.into(),
            name: text(props, "Name"),
            rssi: props.get("RSSI").and_then(|v| v.0.as_i64()).unwrap_or(-127) as i32,
            services,
            manufacturer_data: manufacturer_data(props),
            connectable: None,
        };
        self.seen.insert(path.into());
        self.shared.emit(Event::Advertisement { advertisement });
    }
    pub(crate) fn perform(
        &mut self,
        request: &Value,
        cancelled: &AtomicBool,
    ) -> Result<Value, Error> {
        let op = field(request, "op")?;
        if op == "stop_scan" {
            if self.scanning {
                self.method(&self.adapter, ADAPTER, "StopDiscovery")?;
                self.scanning = false;
            }
            return Ok(Value::Null);
        }
        self.refresh()?;
        if !self
            .props(&self.adapter, ADAPTER)
            .is_some_and(|p| boolean(p, "Powered"))
        {
            return Err(error(
                ErrorKind::PoweredOff,
                "Bluetooth adapter is powered off",
            ));
        }
        if op == "scan" {
            if self.scanning {
                self.method(&self.adapter, ADAPTER, "StopDiscovery")?;
                self.scanning = false;
            }
            self.filters = request["services"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(canonical_uuid)
                .collect();
            self.duplicates = request["duplicates"].as_bool().unwrap_or(false);
            let mut filter = PropMap::new();
            filter.insert("Transport".into(), Variant(Box::new("le".to_string())));
            filter.insert("UUIDs".into(), Variant(Box::new(self.filters.clone())));
            filter.insert("DuplicateData".into(), Variant(Box::new(self.duplicates)));
            self.connection
                .with_proxy("org.bluez", self.adapter.as_str(), TIMEOUT)
                .method_call::<(), _, _, _>(ADAPTER, "SetDiscoveryFilter", (filter,))
                .map_err(bus_error)?;
            self.seen.clear();
            self.method(&self.adapter, ADAPTER, "StartDiscovery")?;
            self.scanning = true;
            return Ok(Value::Null);
        }
        let device = field(request, "device")?;
        if self
            .props(device, DEVICE)
            .and_then(|p| text(p, "Adapter"))
            .as_deref()
            != Some(&self.adapter)
        {
            return Err(error(ErrorKind::NotFound, "unknown device on this adapter"));
        }
        if op == "disconnect" {
            self.method(device, DEVICE, "Disconnect")?;
            self.disconnected(device);
            return Ok(Value::Null);
        }
        if op == "connect" {
            if self
                .props(device, DEVICE)
                .is_some_and(|p| p.contains_key("Class"))
            {
                return Err(error(
                    ErrorKind::Unsupported,
                    "this BlueZ backend cannot select the LE bearer of a dual-mode device",
                ));
            }
            self.method(device, DEVICE, "Connect")?;
            self.connected.insert(device.into());
            self.shared.emit(Event::Connected {
                device: device.into(),
            });
            return Ok(Value::Null);
        }
        if !self.connected.contains(device) {
            return Err(error(ErrorKind::Disconnected, "connect the device first"));
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self
            .props(device, DEVICE)
            .is_some_and(|p| boolean(p, "ServicesResolved"))
        {
            if cancelled.load(Ordering::Acquire) {
                return Err(error(ErrorKind::Disconnected, "request cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(error(ErrorKind::Timeout, "GATT discovery timed out"));
            }
            self.connection
                .process(Duration::from_millis(100))
                .map_err(bus_error)?;
            self.tick()?;
            if !self.connected.contains(device) {
                return Err(error(ErrorKind::Disconnected, "device disconnected"));
            }
            self.refresh()?;
        }
        if op == "services" {
            return Ok(json!(self.services(device)));
        }
        let service = canonical_uuid(field(request, "service")?);
        let characteristic = canonical_uuid(field(request, "characteristic")?);
        let path = self.characteristic(device, &service, &characteristic)?;
        let proxy = self
            .connection
            .with_proxy("org.bluez", path.as_str(), TIMEOUT);
        match op {
            "read" => {
                let (value,): (Vec<u8>,) = proxy
                    .method_call(CHARACTERISTIC, "ReadValue", (PropMap::new(),))
                    .map_err(bus_error)?;
                Ok(json!(value))
            }
            "write" => {
                let value: Vec<u8> = serde_json::from_value(request["value"].clone())
                    .map_err(|e| error(ErrorKind::InvalidArgument, e.to_string()))?;
                let mut options = PropMap::new();
                options.insert(
                    "type".into(),
                    Variant(Box::new(
                        if request["with_response"].as_bool().unwrap_or(true) {
                            "request".to_string()
                        } else {
                            "command".to_string()
                        },
                    )),
                );
                proxy
                    .method_call::<(), _, _, _>(CHARACTERISTIC, "WriteValue", (value, options))
                    .map_err(bus_error)?;
                Ok(Value::Null)
            }
            "notify" => {
                if request["enabled"].as_bool().unwrap_or(false) {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        self.notifications.entry(path.clone())
                    {
                        proxy
                            .method_call::<(), _, _, _>(CHARACTERISTIC, "StartNotify", ())
                            .map_err(bus_error)?;
                        entry.insert((device.into(), service, characteristic));
                    }
                } else if self.notifications.contains_key(&path) {
                    proxy
                        .method_call::<(), _, _, _>(CHARACTERISTIC, "StopNotify", ())
                        .map_err(bus_error)?;
                    self.notifications.remove(&path);
                }
                Ok(Value::Null)
            }
            _ => Err(error(
                ErrorKind::InvalidArgument,
                "unknown Bluetooth operation",
            )),
        }
    }
    fn services(&self, device: &str) -> Vec<Service> {
        let mut services: Vec<_> = self
            .objects
            .iter()
            .filter_map(|(path, interfaces)| {
                let props = interfaces.get(SERVICE)?;
                if text(props, "Device").as_deref() != Some(device) {
                    return None;
                }
                let uuid = text(props, "UUID")?;
                let mut characteristics: Vec<_> = self
                    .objects
                    .values()
                    .filter_map(|interfaces| {
                        let props = interfaces.get(CHARACTERISTIC)?;
                        if text(props, "Service").as_deref() != Some(path.as_ref()) {
                            return None;
                        }
                        let flags = strings(props, "Flags");
                        let has = |flag: &str| flags.iter().any(|s| s == flag);
                        Some(Characteristic {
                            uuid: text(props, "UUID")?,
                            read: has("read"),
                            write: has("write"),
                            write_without_response: has("write-without-response"),
                            notify: has("notify"),
                            indicate: has("indicate"),
                        })
                    })
                    .collect();
                characteristics.sort_by(|a, b| a.uuid.cmp(&b.uuid));
                Some(Service {
                    uuid,
                    characteristics,
                })
            })
            .collect();
        services.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        services
    }
    fn characteristic(
        &self,
        device: &str,
        service: &str,
        characteristic: &str,
    ) -> Result<String, Error> {
        let paths: Vec<_> = self
            .objects
            .iter()
            .filter_map(|(path, interfaces)| {
                let props = interfaces.get(CHARACTERISTIC)?;
                if text(props, "UUID")?.to_lowercase() != characteristic {
                    return None;
                }
                let parent = text(props, "Service")?;
                let service_props = self.props(&parent, SERVICE)?;
                if text(service_props, "Device")?.as_str() != device
                    || text(service_props, "UUID")?.to_lowercase() != service
                {
                    return None;
                }
                Some(path.to_string())
            })
            .collect();
        match paths.as_slice() {
            [path] => Ok(path.clone()),
            [] => Err(error(ErrorKind::NotFound, "characteristic not found")),
            _ => Err(error(
                ErrorKind::InvalidArgument,
                "duplicate GATT UUIDs require instance addressing",
            )),
        }
    }
    pub(crate) fn lost(&mut self) {
        for device in self.connected.clone() {
            self.disconnected(&device);
        }
    }
    pub(crate) fn abort(&mut self, request: &Value) {
        if request["op"] == "scan" && self.scanning {
            let _ = self.method(&self.adapter, ADAPTER, "StopDiscovery");
            self.scanning = false;
        }
        if let Some(device) = request["device"].as_str() {
            // Also covers Connect timing out before this client records a connection.
            if self.props(device, DEVICE).is_some() {
                let _ = self.method(device, DEVICE, "Disconnect");
                self.disconnected(device);
            }
        }
    }
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    value[name]
        .as_str()
        .ok_or_else(|| error(ErrorKind::InvalidArgument, format!("missing {name}")))
}
fn text(props: &PropMap, key: &str) -> Option<String> {
    props.get(key)?.0.as_str().map(str::to_owned)
}
fn boolean(props: &PropMap, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| v.0.as_i64())
        .is_some_and(|n| n != 0)
}
fn strings(props: &PropMap, key: &str) -> Vec<String> {
    props
        .get(key)
        .and_then(|v| v.0.as_iter())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn bytes(props: &PropMap, key: &str) -> Option<Vec<u8>> {
    Some(
        props
            .get(key)?
            .0
            .as_iter()?
            .filter_map(|v| v.as_u64().map(|n| n as u8))
            .collect(),
    )
}
fn manufacturer_data(props: &PropMap) -> Vec<u8> {
    let Some(mut entries) = props.get("ManufacturerData").and_then(|v| v.0.as_iter()) else {
        return Vec::new();
    };
    let Some(company) = entries.next().and_then(RefArg::as_u64) else {
        return Vec::new();
    };
    let Some(value) = entries
        .next()
        .and_then(|v| v.as_iter())
        .and_then(|mut i| i.next())
    else {
        return Vec::new();
    };
    let mut bytes = (company as u16).to_le_bytes().to_vec();
    bytes.extend(
        value
            .as_iter()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_u64().map(|n| n as u8)),
    );
    bytes
}
fn canonical_uuid(value: &str) -> String {
    match value.len() {
        4 => format!("0000{value}-0000-1000-8000-00805f9b34fb"),
        8 => format!("{value}-0000-1000-8000-00805f9b34fb"),
        _ => value.into(),
    }
    .to_lowercase()
}
fn bus_error(e: dbus::Error) -> Error {
    let kind = match e.name().unwrap_or("") {
        "org.bluez.Error.NotAuthorized"
        | "org.bluez.Error.NotPermitted"
        | "org.freedesktop.DBus.Error.AccessDenied" => ErrorKind::PermissionDenied,
        "org.bluez.Error.NotReady" => ErrorKind::PoweredOff,
        "org.bluez.Error.InProgress" => ErrorKind::Busy,
        "org.bluez.Error.NotConnected" => ErrorKind::Disconnected,
        "org.bluez.Error.NotSupported" | "org.freedesktop.DBus.Error.ServiceUnknown" => {
            ErrorKind::Unsupported
        }
        "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
            ErrorKind::Timeout
        }
        _ => ErrorKind::Platform,
    };
    error(kind, e.to_string())
}
