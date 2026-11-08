//! Manual native-host test. Start test-peripheral/run-macos.sh on a separate Mac first.
//! Only connects to the synthetic fixture UUID; never chooses an arbitrary nearby device.
use day_part_bluetooth as ble;
use std::future::Future;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

const SERVICE: &str = "e55733e0-da7b-4c45-ac2d-5d559a122f01";
const VALUE: &str = "e55733e1-da7b-4c45-ac2d-5d559a122f01";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (devices, found) = mpsc::channel();
    let (values, received) = mpsc::channel();
    let _watch = ble::watch(move |event| match event {
        ble::Event::Advertisement { advertisement }
            if advertisement
                .services
                .iter()
                .any(|s| s.eq_ignore_ascii_case(SERVICE)) =>
        {
            println!("Fixture advertisement: RSSI {} dBm", advertisement.rssi);
            let _ = devices.send(advertisement.device);
        }
        ble::Event::Value {
            service,
            characteristic,
            value,
            ..
        } if service.eq_ignore_ascii_case(SERVICE)
            && characteristic.eq_ignore_ascii_case(VALUE) =>
        {
            let _ = values.send(value);
        }
        ble::Event::State { state } => println!("Adapter: {state:?}"),
        ble::Event::Error { error } => eprintln!("Bluetooth event: {error}"),
        _ => {}
    })?;
    block_on(ble::request_access())?;
    block_on(ble::start_scan(&ble::ScanOptions {
        services: vec![SERVICE.into()],
        duplicates: false,
    }))?;
    let discovered = found.recv_timeout(Duration::from_secs(20));
    block_on(ble::stop_scan())?;
    let device = discovered?;
    println!("Software fixture discovered: {device}");
    block_on(ble::connect(&device))?;
    println!("Connected to software fixture");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let services = block_on(ble::services(&device))?;
        println!("Discovered {} GATT services", services.len());
        if !services
            .iter()
            .any(|s| s.uuid.eq_ignore_ascii_case(SERVICE))
        {
            return Err("fixture GATT service missing".into());
        }
        let initial = block_on(ble::read(&device, SERVICE, VALUE))?;
        if !initial.starts_with(b"day-fixture") {
            return Err("unexpected fixture value".into());
        }
        block_on(ble::write(
            &device,
            SERVICE,
            VALUE,
            b"day-fixture-write",
            true,
        ))?;
        if block_on(ble::read(&device, SERVICE, VALUE))? != b"day-fixture-write" {
            return Err("write/read mismatch".into());
        }
        block_on(ble::set_notify(&device, SERVICE, VALUE, true))?;
        block_on(ble::write(
            &device,
            SERVICE,
            VALUE,
            b"day-fixture-notify",
            true,
        ))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if received.recv_timeout(remaining)? == b"day-fixture-notify" {
                break;
            }
        }
        block_on(ble::set_notify(&device, SERVICE, VALUE, false))?;
        println!("GATT discovery, read, write/read, and notification passed");
        Ok(())
    })();
    let disconnected = block_on(ble::disconnect(&device));
    result?;
    disconnected?;
    println!("Disconnected; software-peripheral roundtrip passed");
    Ok(())
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "Bluetooth operation exceeded test deadline"
        );
        std::thread::park_timeout(Duration::from_millis(100));
    }
}
