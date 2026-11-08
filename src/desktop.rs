// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Serialized desktop I/O, with Day bridge completions and cooperative cancellation.
#[cfg(target_os = "linux")]
use crate::bluez::Bluez as Backend;
#[cfg(target_os = "windows")]
use crate::winrt::Winrt as Backend;
use crate::{ErrorKind, Event, error};
use day_bridge::{Done, Emit};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

struct Request {
    value: Value,
    done: Done<String>,
    cancelled: Arc<AtomicBool>,
}
#[derive(Default)]
pub(crate) struct Shared {
    watchers: Mutex<HashMap<u64, Emit<String>>>,
    pending: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    refresh_state: AtomicBool,
}
impl Shared {
    pub(crate) fn emit(&self, event: Event) {
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        let watchers: Vec<_> = self
            .watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for emit in watchers {
            emit.emit(json.clone());
        }
    }
}
struct Runtime {
    shared: Arc<Shared>,
    tx: mpsc::SyncSender<Request>,
}
fn runtime() -> Result<&'static Runtime, day_bridge::Error> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let shared = Arc::new(Shared::default());
            let worker_shared = shared.clone();
            let (tx, rx) = mpsc::sync_channel(128);
            std::thread::Builder::new()
                .name("day-bluetooth-desktop".into())
                .spawn(move || {
                    let mut backend: Option<Backend> = None;
                    loop {
                        if worker_shared.refresh_state.swap(false, Ordering::AcqRel) {
                            if backend.is_none() {
                                match Backend::new(worker_shared.clone()) {
                                    Ok(ready) => backend = Some(ready),
                                    Err(error) => worker_shared.emit(Event::Error { error }),
                                }
                            } else if let Some(backend) = &backend {
                                backend.adapter_state();
                            }
                        }
                        if let Some(active) = backend.as_mut()
                            && let Err(error) = active.tick()
                        {
                            active.lost();
                            worker_shared.emit(Event::Error { error });
                            backend = None;
                        }
                        match rx.recv_timeout(Duration::from_millis(25)) {
                            Ok(Request {
                                value,
                                done,
                                cancelled,
                            }) => {
                                let token = done.token();
                                let result = if cancelled.load(Ordering::Acquire) {
                                    Err(error(ErrorKind::Disconnected, "request cancelled"))
                                } else {
                                    if backend.is_none() {
                                        match Backend::new(worker_shared.clone()) {
                                            Ok(ready) => backend = Some(ready),
                                            Err(failure) => {
                                                worker_shared.emit(Event::Error {
                                                    error: failure.clone(),
                                                });
                                                done.complete(Ok(
                                                    json!({"error":failure}).to_string()
                                                ));
                                                worker_shared
                                                    .pending
                                                    .lock()
                                                    .unwrap_or_else(|e| e.into_inner())
                                                    .remove(&token);
                                                continue;
                                            }
                                        }
                                    }
                                    let backend = backend.as_mut().expect("initialized above");
                                    let result = backend.perform(&value, &cancelled);
                                    if cancelled.load(Ordering::Acquire)
                                        || matches!(&result, Err(e) if e.kind == ErrorKind::Timeout)
                                    {
                                        backend.abort(&value);
                                    }
                                    result
                                };
                                worker_shared
                                    .pending
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .remove(&token);
                                done.complete(Ok(match result {
                                    Ok(result) => json!({"result":result}),
                                    Err(error) => json!({"error":error}),
                                }
                                .to_string()));
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(Runtime { shared, tx })
        })
        .as_ref()
        .map_err(|e| day_bridge::Error::Foreign(e.clone()))
}

pub(crate) fn request(text: &str, done: Done<String>) -> Result<(), day_bridge::Error> {
    let value =
        serde_json::from_str(text).map_err(|e| day_bridge::Error::Foreign(e.to_string()))?;
    let runtime = runtime()?;
    let token = done.token();
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut pending = runtime
        .shared
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if pending.len() >= 128 {
        drop(pending);
        done.complete(Ok(
            json!({"error":error(ErrorKind::Busy,"Bluetooth request queue is full")}).to_string(),
        ));
        return Ok(());
    }
    pending.insert(token, cancelled.clone());
    if let Err(failure) = runtime.tx.try_send(Request {
        value,
        done,
        cancelled,
    }) {
        pending.remove(&token);
        drop(pending);
        let request = match failure {
            mpsc::TrySendError::Full(r) | mpsc::TrySendError::Disconnected(r) => r,
        };
        request.done.complete(Ok(
            json!({"error":error(ErrorKind::Busy,"Bluetooth worker is unavailable")}).to_string(),
        ));
    }
    Ok(())
}
pub(crate) fn watch(emit: Emit<String>) -> Result<(), day_bridge::Error> {
    let runtime = runtime()?;
    emit.emit(json!({"event":"state","state":"unknown"}).to_string());
    runtime
        .shared
        .watchers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(emit.token(), emit);
    runtime.shared.refresh_state.store(true, Ordering::Release);
    Ok(())
}
pub(crate) fn cancel(token: u64) {
    if let Ok(runtime) = runtime()
        && let Some(flag) = runtime
            .shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&token)
    {
        flag.store(true, Ordering::Release);
    }
}
pub(crate) fn unwatch(token: u64) {
    if let Ok(runtime) = runtime() {
        runtime
            .shared
            .watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&token);
    }
}
