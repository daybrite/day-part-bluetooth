//! Run with `cargo run --example mock_peripheral`. No Bluetooth device or OS permission needed.
use day_part_bluetooth::{
    self as ble,
    simulation::{Reply, Request, Simulation},
};
use std::future::Future;
use std::task::{Context, Poll, Waker};

// Application code: these are the same calls used for a real battery service.
async fn read_battery(device: &str) -> Result<Vec<u8>, ble::Error> {
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
fn main() -> Result<(), ble::Error> {
    let simulation = Simulation::new();
    simulation.expect(Request::RequestAccess, Ok(Reply::Unit));
    simulation.expect(
        Request::Connect {
            device: "test-battery".into(),
        },
        Ok(Reply::Unit),
    );
    simulation.expect(
        Request::Services {
            device: "test-battery".into(),
        },
        Ok(Reply::Services(vec![ble::Service {
            uuid: "180f".into(),
            characteristics: vec![ble::Characteristic {
                uuid: "2a19".into(),
                read: true,
                write: false,
                write_without_response: false,
                notify: false,
                indicate: false,
            }],
        }])),
    );
    simulation.expect(
        Request::Read {
            device: "test-battery".into(),
            service: "180f".into(),
            characteristic: "2a19".into(),
        },
        Ok(Reply::Value(vec![73])),
    );
    simulation.expect(
        Request::Disconnect {
            device: "test-battery".into(),
        },
        Ok(Reply::Unit),
    );
    let _interception = simulation.install();
    // Scripted replies complete immediately, so this example needs no async runtime.
    let mut reading = std::pin::pin!(read_battery("test-battery"));
    match reading
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => assert_eq!(result?, vec![73]),
        Poll::Pending => panic!("scripted reply unexpectedly waited"),
    }
    simulation.assert_finished()?;
    println!("Mock battery read passed through the standard Bluetooth API; no radio used.");
    Ok(())
}
