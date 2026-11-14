// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Only talks to the synthetic peripheral in ../test-peripheral.
use day_part_bluetooth as ble;
use std::sync::{Arc, Mutex};

/// Canonical spellings, as every UUID the package returns is.
const SERVICE: &str = "e55733e0-da7b-4c45-ac2d-5d559a122f01";
const VALUE: &str = "e55733e1-da7b-4c45-ac2d-5d559a122f01";
const WRITE: &[u8] = b"day-fixture-write";
const NOTIFY: &[u8] = b"day-fixture-notify";

fn error(message: &str) -> ble::Error {
    ble::Error {
        kind: ble::ErrorKind::Platform,
        message: message.into(),
    }
}
pub async fn roundtrip() -> Result<(), ble::Error> {
    let found = Arc::new(Mutex::new(None::<String>));
    let messages = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let advertised = found.clone();
    let received = messages.clone();
    let _watch = ble::watch(move |event| match event {
        ble::Event::Advertisement { advertisement }
            if advertisement.services.iter().any(|id| id == SERVICE) =>
        {
            *advertised.lock().unwrap() = Some(advertisement.device);
        }
        ble::Event::Value {
            service,
            characteristic,
            value,
            ..
        } if service == SERVICE && characteristic == VALUE => {
            received.lock().unwrap().push(value);
        }
        _ => {}
    })?;
    ble::request_access().await?;
    ble::start_scan(&ble::ScanOptions {
        services: vec![SERVICE.into()],
        duplicates: false,
    })
    .await?;
    let mut device = None;
    for _ in 0..100 {
        device.clone_from(&found.lock().unwrap());
        if device.is_some() {
            break;
        }
        day::sleep(100).await;
    }
    ble::stop_scan().await?;
    let device = device.ok_or_else(|| error("software peripheral was not found"))?;
    ble::connect(&device).await?;
    let result = async {
        let services = ble::services(&device).await?;
        if !services.iter().any(|s| s.uuid == SERVICE) {
            return Err(error("fixture service missing"));
        }
        let limit = ble::max_write_length(&device, true).await?;
        if limit < NOTIFY.len() {
            return Err(error("write limit below the fixture payload"));
        }
        let initial = ble::read(&device, SERVICE, VALUE).await?;
        if !initial.starts_with(b"day-fixture") {
            return Err(error("unexpected fixture value"));
        }
        ble::write(&device, SERVICE, VALUE, WRITE, true).await?;
        if ble::read(&device, SERVICE, VALUE).await? != WRITE {
            return Err(error("write/read mismatch"));
        }
        // A read answer must not arrive as a value event.
        if !messages.lock().unwrap().is_empty() {
            return Err(error("read answer was delivered as a value event"));
        }
        ble::set_notify(&device, SERVICE, VALUE, true).await?;
        ble::write(&device, SERVICE, VALUE, NOTIFY, true).await?;
        for _ in 0..50 {
            let notified = messages.lock().unwrap().iter().any(|v| v == NOTIFY);
            if notified {
                ble::set_notify(&device, SERVICE, VALUE, false).await?;
                return Ok(());
            }
            day::sleep(100).await;
        }
        Err(error("notification was not received"))
    }
    .await;
    let disconnected = ble::disconnect(&device).await;
    result.and(disconnected)
}
