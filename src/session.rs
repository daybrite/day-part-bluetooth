// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Isolated Bluetooth routing. Existing free functions use the global session.
use crate::{
    Error, ErrorKind, Event, ScanOptions, Service, Watch, error,
    simulation::{Reply, Request, Simulation},
};
use std::sync::{Arc, Mutex, OnceLock, Weak};
#[derive(Default)]
struct Settings {
    simulation: Option<Simulation>,
    overrides: Vec<(u64, Simulation)>,
    next_id: u64,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
/// Clones share routing settings; a new session defaults to native Bluetooth and does not
/// inherit global interception. Native sessions share the platform adapter; simulated
/// sessions are isolated when they use separate `Simulation` instances.
/// Async operations capture their selection when first polled.
#[derive(Clone, Default)]
pub struct Session(Arc<Mutex<Settings>>);
impl Session {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn global() -> Self {
        static GLOBAL: OnceLock<Session> = OnceLock::new();
        GLOBAL.get_or_init(Self::new).clone()
    }
    /// Changes the base selection for subsequent operations. Scoped overrides take priority.
    /// In-flight operations and watches retain their selected simulation. Stop watches and
    /// disconnect before switching modes, since a BLE connection spans several operations.
    pub fn set_simulation(&self, simulation: Option<Simulation>) {
        lock(&self.0).simulation = simulation;
    }
    pub fn install_simulation(&self, simulation: Simulation) -> SimulationGuard {
        let mut settings = lock(&self.0);
        settings.next_id = settings
            .next_id
            .checked_add(1)
            .expect("Bluetooth override IDs exhausted");
        let id = settings.next_id;
        settings.overrides.push((id, simulation));
        SimulationGuard {
            settings: Arc::downgrade(&self.0),
            id,
        }
    }
    fn simulation(&self) -> Option<Simulation> {
        let settings = lock(&self.0);
        settings
            .overrides
            .last()
            .map(|(_, s)| s.clone())
            .or_else(|| settings.simulation.clone())
    }
    pub fn supported(&self) -> bool {
        self.simulation().is_some() || crate::native_supported()
    }
    pub fn watch(&self, callback: impl FnMut(Event) + Send + 'static) -> Result<Watch, Error> {
        match self.simulation() {
            Some(simulation) => Ok(simulation.watch(callback)),
            None => crate::native_watch(callback),
        }
    }
    pub async fn request_access(&self) -> Result<(), Error> {
        match self.simulation() {
            Some(simulation) => unit(simulation.dispatch(Request::RequestAccess).await?),
            None => crate::native_request_access().await,
        }
    }
    pub async fn start_scan(&self, options: &ScanOptions) -> Result<(), Error> {
        for uuid in &options.services {
            crate::validate_uuid(uuid)?;
        }
        match self.simulation() {
            Some(simulation) => unit(
                simulation
                    .dispatch(Request::StartScan(options.clone()))
                    .await?,
            ),
            None => crate::native_start_scan(options).await,
        }
    }
    pub async fn stop_scan(&self) -> Result<(), Error> {
        match self.simulation() {
            Some(s) => unit(s.dispatch(Request::StopScan).await?),
            None => crate::native_stop_scan().await,
        }
    }
    pub async fn connect(&self, device: &str) -> Result<(), Error> {
        match self.simulation() {
            Some(s) => unit(
                s.dispatch(Request::Connect {
                    device: device.into(),
                })
                .await?,
            ),
            None => crate::native_connect(device).await,
        }
    }
    pub async fn disconnect(&self, device: &str) -> Result<(), Error> {
        match self.simulation() {
            Some(s) => unit(
                s.dispatch(Request::Disconnect {
                    device: device.into(),
                })
                .await?,
            ),
            None => crate::native_disconnect(device).await,
        }
    }
    pub async fn services(&self, device: &str) -> Result<Vec<Service>, Error> {
        match self.simulation() {
            Some(s) => match s
                .dispatch(Request::Services {
                    device: device.into(),
                })
                .await?
            {
                Reply::Services(services) => Ok(services),
                _ => Err(wrong_reply()),
            },
            None => crate::native_services(device).await,
        }
    }
    pub async fn read(
        &self,
        device: &str,
        service: &str,
        characteristic: &str,
    ) -> Result<Vec<u8>, Error> {
        validate(service, characteristic)?;
        match self.simulation() {
            Some(s) => match s
                .dispatch(Request::Read {
                    device: device.into(),
                    service: service.into(),
                    characteristic: characteristic.into(),
                })
                .await?
            {
                Reply::Value(bytes) => Ok(bytes),
                _ => Err(wrong_reply()),
            },
            None => crate::native_read(device, service, characteristic).await,
        }
    }
    pub async fn write(
        &self,
        device: &str,
        service: &str,
        characteristic: &str,
        value: &[u8],
        with_response: bool,
    ) -> Result<(), Error> {
        validate(service, characteristic)?;
        match self.simulation() {
            Some(s) => unit(
                s.dispatch(Request::Write {
                    device: device.into(),
                    service: service.into(),
                    characteristic: characteristic.into(),
                    value: value.to_vec(),
                    with_response,
                })
                .await?,
            ),
            None => {
                crate::native_write(device, service, characteristic, value, with_response).await
            }
        }
    }
    pub async fn set_notify(
        &self,
        device: &str,
        service: &str,
        characteristic: &str,
        enabled: bool,
    ) -> Result<(), Error> {
        validate(service, characteristic)?;
        match self.simulation() {
            Some(s) => unit(
                s.dispatch(Request::SetNotify {
                    device: device.into(),
                    service: service.into(),
                    characteristic: characteristic.into(),
                    enabled,
                })
                .await?,
            ),
            None => crate::native_set_notify(device, service, characteristic, enabled).await,
        }
    }
    pub async fn max_write_length(
        &self,
        device: &str,
        with_response: bool,
    ) -> Result<usize, Error> {
        match self.simulation() {
            Some(s) => match s
                .dispatch(Request::MaxWriteLength {
                    device: device.into(),
                    with_response,
                })
                .await?
            {
                Reply::Length(length) => Ok(length),
                _ => Err(wrong_reply()),
            },
            None => crate::native_max_write_length(device, with_response).await,
        }
    }
}
/// Restores the previous routing selection on drop; never cancels an already selected request
/// or watch. Keep the guard for the whole test/app session, not just for fixture construction.
#[must_use = "keep this guard alive while the Bluetooth override is needed"]
pub struct SimulationGuard {
    settings: Weak<Mutex<Settings>>,
    id: u64,
}
impl Drop for SimulationGuard {
    fn drop(&mut self) {
        if let Some(settings) = self.settings.upgrade() {
            lock(&settings).overrides.retain(|(id, _)| *id != self.id);
        }
    }
}
fn unit(reply: Reply) -> Result<(), Error> {
    if reply == Reply::Unit {
        Ok(())
    } else {
        Err(wrong_reply())
    }
}
fn wrong_reply() -> Error {
    error(
        ErrorKind::InvalidArgument,
        "Bluetooth simulation returned the wrong reply type",
    )
}
fn validate(service: &str, characteristic: &str) -> Result<(), Error> {
    crate::validate_uuid(service)?;
    crate::validate_uuid(characteristic)
}
