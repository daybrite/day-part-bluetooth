//! Only talks to the synthetic peripheral in ../test-peripheral.
use day_part_bluetooth as ble;
use std::sync::{Arc, Mutex};
const SERVICE: &str = "E55733E0-DA7B-4C45-AC2D-5D559A122F01";
const VALUE: &str = "E55733E1-DA7B-4C45-AC2D-5D559A122F01";
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
            if advertisement
                .services
                .iter()
                .any(|id| id.eq_ignore_ascii_case(SERVICE)) =>
        {
            *advertised.lock().unwrap() = Some(advertisement.device);
        }
        ble::Event::Value {
            service,
            characteristic,
            value,
            ..
        } if service.eq_ignore_ascii_case(SERVICE)
            && characteristic.eq_ignore_ascii_case(VALUE) =>
        {
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
        device = found.lock().unwrap().clone();
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
        if !services
            .iter()
            .any(|s| s.uuid.eq_ignore_ascii_case(SERVICE))
        {
            return Err(error("fixture service missing"));
        }
        let initial = ble::read(&device, SERVICE, VALUE).await?;
        if !initial.starts_with(b"day-fixture") {
            return Err(error("unexpected fixture value"));
        }
        ble::write(&device, SERVICE, VALUE, b"day-fixture-write", true).await?;
        if ble::read(&device, SERVICE, VALUE).await? != b"day-fixture-write" {
            return Err(error("write/read mismatch"));
        }
        ble::set_notify(&device, SERVICE, VALUE, true).await?;
        ble::write(&device, SERVICE, VALUE, b"day-fixture-notify", true).await?;
        for _ in 0..50 {
            let notified = messages
                .lock()
                .unwrap()
                .iter()
                .any(|v| v == b"day-fixture-notify");
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
