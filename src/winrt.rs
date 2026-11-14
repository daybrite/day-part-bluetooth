// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! WinRT BLE on a dedicated MTA worker. Bounded waits bridge Windows operations into Day;
//! no Tokio or separate Rust async executor is required.
use crate::desktop::Shared;
use crate::{AdapterState, Advertisement, Characteristic, Error, ErrorKind, Event, Service, error};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
    BluetoothLEScanningMode,
};
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattCharacteristicProperties,
    GattClientCharacteristicConfigurationDescriptorValue as Cccd, GattCommunicationStatus,
    GattDeviceService, GattSession, GattValueChangedEventArgs, GattWriteOption,
};
use windows::Devices::Bluetooth::{
    BluetoothAdapter, BluetoothAddressType, BluetoothCacheMode, BluetoothConnectionStatus,
    BluetoothLEDevice,
};
use windows::Devices::Radios::{Radio, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{GUID, RuntimeType};
use windows_future::{AsyncStatus, IAsyncOperation};

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, Error> {
        // This package owns the worker thread and its apartment for the backend's lifetime.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(native_error)?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

#[derive(Default)]
struct Scan {
    active: bool,
    filters: Vec<String>,
    duplicates: bool,
    seen: HashSet<String>,
    addresses: HashMap<String, (u64, BluetoothAddressType)>,
}
struct Subscription {
    characteristic: GattCharacteristic,
    token: i64,
    active: Arc<AtomicBool>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let _ = self.characteristic.RemoveValueChanged(self.token);
    }
}
struct Peripheral {
    device: BluetoothLEDevice,
    changed: i64,
    alive: Arc<AtomicBool>,
    services: Vec<GattDeviceService>,
    description: Vec<Service>,
    characteristics: HashMap<(String, String), Vec<GattCharacteristic>>,
    notifications: HashMap<(String, String), Subscription>,
}
impl Drop for Peripheral {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.notifications.clear();
        let _ = self.device.RemoveConnectionStatusChanged(self.changed);
        for service in &self.services {
            let _ = service.Close();
        }
        let _ = self.device.Close();
    }
}
pub(crate) struct Winrt {
    radio: Radio,
    watcher: BluetoothLEAdvertisementWatcher,
    received: i64,
    scan: Arc<Mutex<Scan>>,
    connections: HashMap<String, Peripheral>,
    shared: Arc<Shared>,
    state: AdapterState,
    next_radio_poll: Instant,
    // Declared last so WinRT objects are released before the apartment is uninitialized.
    _apartment: Apartment,
}
impl Drop for Winrt {
    fn drop(&mut self) {
        self.scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active = false;
        let _ = self.watcher.Stop();
        let _ = self.watcher.RemoveReceived(self.received);
        self.connections.clear();
    }
}
impl Winrt {
    pub(crate) fn new(shared: Arc<Shared>) -> Result<Self, Error> {
        let apartment = Apartment::new()?;
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(15);
        let adapter = wait(
            BluetoothAdapter::GetDefaultAsync().map_err(native_error)?,
            &cancelled,
            deadline,
        )
        .map_err(|e| {
            if e.kind == ErrorKind::Platform {
                error(
                    ErrorKind::Unsupported,
                    "no usable Windows Bluetooth adapter",
                )
            } else {
                e
            }
        })?;
        if !adapter.IsLowEnergySupported().map_err(native_error)? {
            return Err(error(ErrorKind::Unsupported, "Bluetooth LE is unavailable"));
        }
        let radio = wait(
            adapter.GetRadioAsync().map_err(native_error)?,
            &cancelled,
            deadline,
        )?;
        let watcher = BluetoothLEAdvertisementWatcher::new().map_err(native_error)?;
        watcher
            .SetScanningMode(BluetoothLEScanningMode::Active)
            .map_err(native_error)?;
        let scan = Arc::new(Mutex::new(Scan::default()));
        let received_scan = scan.clone();
        let events = shared.clone();
        let received = watcher
            .Received(&TypedEventHandler::<
                BluetoothLEAdvertisementWatcher,
                BluetoothLEAdvertisementReceivedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    match advertisement(args, &received_scan) {
                        Ok(Some(advertisement)) => {
                            events.emit(Event::Advertisement { advertisement });
                        }
                        Err(error) => events.emit(Event::Error { error }),
                        _ => {}
                    }
                }
                Ok(())
            }))
            .map_err(native_error)?;
        let mut backend = Self {
            radio,
            watcher,
            received,
            scan,
            connections: HashMap::new(),
            shared,
            state: AdapterState::Unknown,
            next_radio_poll: Instant::now(),
            _apartment: apartment,
        };
        backend.tick()?;
        Ok(backend)
    }
    pub(crate) fn adapter_state(&self) {
        self.shared.emit(Event::State {
            state: self.state.clone(),
        });
    }
    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        self.connections
            .retain(|_, connection| connection.alive.load(Ordering::Acquire));
        if Instant::now() < self.next_radio_poll {
            return Ok(());
        }
        self.next_radio_poll = Instant::now() + Duration::from_secs(1);
        let state = match self.radio.State().map_err(native_error)? {
            RadioState::On => AdapterState::PoweredOn,
            RadioState::Off | RadioState::Disabled => AdapterState::PoweredOff,
            _ => AdapterState::Unknown,
        };
        if state != self.state {
            self.state = state;
            self.adapter_state();
            if self.state != AdapterState::PoweredOn {
                self.scan
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .active = false;
                self.lost();
            }
        }
        Ok(())
    }
    pub(crate) fn lost(&mut self) {
        for (device, connection) in self.connections.drain() {
            if connection.alive.swap(false, Ordering::AcqRel) {
                self.shared.emit(Event::Disconnected { device });
            }
        }
    }
    pub(crate) fn abort(&mut self, request: &Value) {
        if request["op"] == "scan" {
            self.scan
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active = false;
            let _ = self.watcher.Stop();
        }
        if let Some(device) = request["device"].as_str() {
            self.disconnect(device);
        }
    }
    fn disconnect(&mut self, device: &str) {
        if let Some(connection) = self.connections.remove(device)
            && connection.alive.swap(false, Ordering::AcqRel)
        {
            self.shared.emit(Event::Disconnected {
                device: device.into(),
            });
        }
    }
    pub(crate) fn perform(
        &mut self,
        request: &Value,
        cancelled: &AtomicBool,
    ) -> Result<Value, Error> {
        let op = field(request, "op")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        if op == "stop_scan" {
            self.scan
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active = false;
            self.watcher.Stop().map_err(native_error)?;
            return Ok(Value::Null);
        }
        if op == "disconnect" {
            self.disconnect(field(request, "device")?);
            return Ok(Value::Null);
        }
        self.tick()?;
        if op == "access" {
            return Ok(json!(self.state));
        }
        if self.state != AdapterState::PoweredOn {
            return Err(error(ErrorKind::PoweredOff, "Bluetooth is powered off"));
        }
        if op == "scan" {
            // Ignore callbacks from the previous scan while replacing its filter.
            self.scan
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active = false;
            self.watcher.Stop().map_err(native_error)?;
            {
                let mut scan = self
                    .scan
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                scan.filters = request["services"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(canonical_uuid)
                    .collect();
                scan.duplicates = request["duplicates"].as_bool().unwrap_or(false);
                scan.seen.clear();
                scan.addresses.clear();
                scan.active = true;
            }
            if let Err(e) = self.watcher.Start() {
                self.scan
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .active = false;
                return Err(native_error(e));
            }
            return Ok(Value::Null);
        }
        let device = field(request, "device")?;
        if op == "connect" {
            if self.connections.contains_key(device) {
                return Ok(Value::Null);
            }
            if self.connections.len() >= 32 {
                return Err(error(ErrorKind::Busy, "connection limit reached"));
            }
            let (address, address_type) = self
                .scan
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .addresses
                .get(device)
                .copied()
                .ok_or_else(|| {
                    error(ErrorKind::NotFound, "scan for the device before connecting")
                })?;
            let peripheral = connect(
                device,
                address,
                address_type,
                cancelled,
                deadline,
                self.shared.clone(),
            )?;
            self.connections.insert(device.into(), peripheral);
            self.shared.emit(Event::Connected {
                device: device.into(),
            });
            return Ok(Value::Null);
        }
        let peripheral = self
            .connections
            .get_mut(device)
            .filter(|p| p.alive.load(Ordering::Acquire))
            .ok_or_else(|| error(ErrorKind::Disconnected, "connect the device first"))?;
        if op == "services" {
            return Ok(json!(peripheral.description));
        }
        if op == "max_write" {
            let session = wait(
                GattSession::FromDeviceIdAsync(
                    &peripheral
                        .device
                        .BluetoothDeviceId()
                        .map_err(native_error)?,
                )
                .map_err(native_error)?,
                cancelled,
                deadline,
            )?;
            let mtu = session.MaxPduSize().map_err(native_error)? as usize;
            return Ok(json!(mtu.saturating_sub(3)));
        }
        let key = (
            canonical_uuid(field(request, "service")?),
            canonical_uuid(field(request, "characteristic")?),
        );
        let characteristic = match peripheral.characteristics.get(&key).map(Vec::as_slice) {
            Some([value]) => value.clone(),
            Some(_) => {
                return Err(error(
                    ErrorKind::InvalidArgument,
                    "duplicate GATT UUIDs require instance addressing",
                ));
            }
            None => return Err(error(ErrorKind::NotFound, "characteristic not found")),
        };
        match op {
            "read" => {
                let result = wait(
                    characteristic
                        .ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached)
                        .map_err(native_error)?,
                    cancelled,
                    deadline,
                )?;
                check_status(result.Status().map_err(native_error)?)?;
                Ok(json!(buffer_bytes(&result.Value().map_err(native_error)?)?))
            }
            "write" => {
                let bytes: Vec<u8> = serde_json::from_value(request["value"].clone())
                    .map_err(|e| error(ErrorKind::InvalidArgument, e.to_string()))?;
                let writer = DataWriter::new().map_err(native_error)?;
                writer.WriteBytes(&bytes).map_err(native_error)?;
                let buffer = writer.DetachBuffer().map_err(native_error)?;
                let option = if request["with_response"].as_bool().unwrap_or(true) {
                    GattWriteOption::WriteWithResponse
                } else {
                    GattWriteOption::WriteWithoutResponse
                };
                check_status(wait(
                    characteristic
                        .WriteValueWithOptionAsync(&buffer, option)
                        .map_err(native_error)?,
                    cancelled,
                    deadline,
                )?)?;
                Ok(Value::Null)
            }
            "notify" => {
                if request["enabled"].as_bool().unwrap_or(false) {
                    if peripheral.notifications.contains_key(&key) {
                        return Ok(Value::Null);
                    }
                    let properties = characteristic
                        .CharacteristicProperties()
                        .map_err(native_error)?;
                    let cccd = if properties.contains(GattCharacteristicProperties::Notify) {
                        Cccd::Notify
                    } else if properties.contains(GattCharacteristicProperties::Indicate) {
                        Cccd::Indicate
                    } else {
                        return Err(error(
                            ErrorKind::Unsupported,
                            "characteristic cannot notify or indicate",
                        ));
                    };
                    let active = Arc::new(AtomicBool::new(true));
                    let active_callback = active.clone();
                    let alive = peripheral.alive.clone();
                    let events = self.shared.clone();
                    let owner = device.to_string();
                    let (service, uuid) = key.clone();
                    let token = characteristic
                        .ValueChanged(&TypedEventHandler::<
                            GattCharacteristic,
                            GattValueChangedEventArgs,
                        >::new(move |_, args| {
                            if active_callback.load(Ordering::Acquire)
                                && alive.load(Ordering::Acquire)
                                && let Some(args) = args.as_ref()
                            {
                                let result = args
                                    .CharacteristicValue()
                                    .map_err(native_error)
                                    .and_then(|b| buffer_bytes(&b));
                                match result {
                                    Ok(value) => events.emit(Event::Value {
                                        device: owner.clone(),
                                        service: service.clone(),
                                        characteristic: uuid.clone(),
                                        value,
                                    }),
                                    Err(error) => events.emit(Event::Error { error }),
                                }
                            }
                            Ok(())
                        }))
                        .map_err(native_error)?;
                    let subscription = Subscription {
                        characteristic: characteristic.clone(),
                        token,
                        active,
                    };
                    check_status(wait(
                        characteristic
                            .WriteClientCharacteristicConfigurationDescriptorAsync(cccd)
                            .map_err(native_error)?,
                        cancelled,
                        deadline,
                    )?)?;
                    peripheral.notifications.insert(key, subscription);
                } else if peripheral.notifications.contains_key(&key) {
                    check_status(wait(
                        characteristic
                            .WriteClientCharacteristicConfigurationDescriptorAsync(Cccd::None)
                            .map_err(native_error)?,
                        cancelled,
                        deadline,
                    )?)?;
                    peripheral.notifications.remove(&key);
                }
                Ok(Value::Null)
            }
            _ => Err(error(
                ErrorKind::InvalidArgument,
                "unknown Bluetooth operation",
            )),
        }
    }
}
fn connect(
    id: &str,
    address: u64,
    address_type: BluetoothAddressType,
    cancelled: &AtomicBool,
    deadline: Instant,
    events: Arc<Shared>,
) -> Result<Peripheral, Error> {
    let device = wait(
        BluetoothLEDevice::FromBluetoothAddressWithBluetoothAddressTypeAsync(address, address_type)
            .map_err(native_error)?,
        cancelled,
        deadline,
    )?;
    let alive = Arc::new(AtomicBool::new(false));
    let watch_alive = alive.clone();
    let owner = id.to_string();
    let changed = match device.ConnectionStatusChanged(&TypedEventHandler::<
        BluetoothLEDevice,
        windows::core::IInspectable,
    >::new(move |sender, _| {
        if let Some(sender) = sender.as_ref()
            && sender.ConnectionStatus()? == BluetoothConnectionStatus::Disconnected
            && watch_alive.swap(false, Ordering::AcqRel)
        {
            events.emit(Event::Disconnected {
                device: owner.clone(),
            });
        }
        Ok(())
    })) {
        Ok(token) => token,
        Err(e) => {
            let _ = device.Close();
            return Err(native_error(e));
        }
    };
    let mut peripheral = Peripheral {
        device,
        changed,
        alive,
        services: Vec::new(),
        description: Vec::new(),
        characteristics: HashMap::new(),
        notifications: HashMap::new(),
    };
    let result = wait(
        peripheral
            .device
            .GetGattServicesWithCacheModeAsync(BluetoothCacheMode::Uncached)
            .map_err(native_error)?,
        cancelled,
        deadline,
    )?;
    check_status(result.Status().map_err(native_error)?)?;
    for service in result.Services().map_err(native_error)? {
        // Retain the service before further fallible calls so every opened handle is closed.
        peripheral.services.push(service.clone());
        let service_uuid = uuid(service.Uuid().map_err(native_error)?);
        let result = wait(
            service
                .GetCharacteristicsWithCacheModeAsync(BluetoothCacheMode::Uncached)
                .map_err(native_error)?,
            cancelled,
            deadline,
        )?;
        check_status(result.Status().map_err(native_error)?)?;
        let mut description = Service {
            uuid: service_uuid.clone(),
            characteristics: Vec::new(),
        };
        for characteristic in result.Characteristics().map_err(native_error)? {
            let uuid = uuid(characteristic.Uuid().map_err(native_error)?);
            let flags = characteristic
                .CharacteristicProperties()
                .map_err(native_error)?;
            description.characteristics.push(Characteristic {
                uuid: uuid.clone(),
                read: flags.contains(GattCharacteristicProperties::Read),
                write: flags.contains(GattCharacteristicProperties::Write),
                write_without_response: flags
                    .contains(GattCharacteristicProperties::WriteWithoutResponse),
                notify: flags.contains(GattCharacteristicProperties::Notify),
                indicate: flags.contains(GattCharacteristicProperties::Indicate),
            });
            peripheral
                .characteristics
                .entry((service_uuid.clone(), uuid))
                .or_default()
                .push(characteristic);
        }
        peripheral.description.push(description);
    }
    peripheral.alive.store(true, Ordering::Release);
    if peripheral.device.ConnectionStatus().map_err(native_error)?
        != BluetoothConnectionStatus::Connected
    {
        return Err(error(
            ErrorKind::Disconnected,
            "device disconnected during discovery",
        ));
    }
    Ok(peripheral)
}
fn advertisement(
    args: &BluetoothLEAdvertisementReceivedEventArgs,
    state: &Mutex<Scan>,
) -> Result<Option<Advertisement>, Error> {
    let advertisement = args.Advertisement().map_err(native_error)?;
    let address = args.BluetoothAddress().map_err(native_error)?;
    let address_type = args.BluetoothAddressType().map_err(native_error)?;
    let device = format!("{:012x}:{}", address, address_type.0);
    let services: Vec<_> = advertisement
        .ServiceUuids()
        .map_err(native_error)?
        .into_iter()
        .map(uuid)
        .collect();
    {
        let mut scan = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !scan.active
            || (!scan.duplicates && scan.seen.contains(&device))
            || (!scan.filters.is_empty() && !services.iter().any(|s| scan.filters.contains(s)))
        {
            return Ok(None);
        }
        // Bound caches for long scans in dense environments. A new scan clears duplicate history.
        if scan.addresses.len() >= 4096 && !scan.addresses.contains_key(&device) {
            return Ok(None);
        }
        scan.addresses
            .insert(device.clone(), (address, address_type));
        scan.seen.insert(device.clone());
    }
    let mut manufacturer_data = Vec::new();
    let manufacturers = advertisement.ManufacturerData().map_err(native_error)?;
    if manufacturers.Size().map_err(native_error)? > 0 {
        let data = manufacturers.GetAt(0).map_err(native_error)?;
        manufacturer_data.extend(data.CompanyId().map_err(native_error)?.to_le_bytes());
        manufacturer_data.extend(buffer_bytes(&data.Data().map_err(native_error)?)?);
    }
    let name = advertisement.LocalName().map_err(native_error)?.to_string();
    Ok(Some(Advertisement {
        device,
        name: (!name.is_empty()).then_some(name),
        rssi: i32::from(args.RawSignalStrengthInDBm().map_err(native_error)?),
        services,
        manufacturer_data,
        connectable: args.IsConnectable().ok(),
    }))
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "owns the operation it drives to completion"
)]
fn wait<T: RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<T, Error> {
    while operation.Status().map_err(native_error)? == AsyncStatus::Started {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = operation.Cancel();
            return Err(error(
                if cancelled.load(Ordering::Acquire) {
                    ErrorKind::Disconnected
                } else {
                    ErrorKind::Timeout
                },
                "Windows Bluetooth operation cancelled or timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    operation.GetResults().map_err(native_error)
}
fn buffer_bytes(buffer: &IBuffer) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![0; buffer.Length().map_err(native_error)? as usize];
    DataReader::FromBuffer(buffer)
        .map_err(native_error)?
        .ReadBytes(&mut bytes)
        .map_err(native_error)?;
    Ok(bytes)
}
fn check_status(status: GattCommunicationStatus) -> Result<(), Error> {
    match status {
        GattCommunicationStatus::Success => Ok(()),
        GattCommunicationStatus::AccessDenied => {
            Err(error(ErrorKind::PermissionDenied, "GATT access denied"))
        }
        GattCommunicationStatus::Unreachable => {
            Err(error(ErrorKind::Disconnected, "GATT device unreachable"))
        }
        _ => Err(error(
            ErrorKind::Platform,
            format!("GATT status {}", status.0),
        )),
    }
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "passed to `map_err`, which hands the error over by value"
)]
fn native_error(e: windows::core::Error) -> Error {
    let kind = if e.code().0 as u32 == 0x8007_0005 {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::Platform
    };
    error(kind, e.to_string())
}
fn uuid(value: GUID) -> String {
    format!("{value:?}").to_lowercase()
}
fn canonical_uuid(value: &str) -> String {
    match value.len() {
        4 => format!("0000{value}-0000-1000-8000-00805f9b34fb"),
        8 => format!("{value}-0000-1000-8000-00805f9b34fb"),
        _ => value.into(),
    }
    .to_lowercase()
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, Error> {
    value[name]
        .as_str()
        .ok_or_else(|| error(ErrorKind::InvalidArgument, format!("missing {name}")))
}
