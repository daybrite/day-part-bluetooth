//! These tests deliberately use synthetic IDs/data and never access native Bluetooth.
use day_part_bluetooth::{
    self as ble,
    simulation::{Reply, Request, Simulation},
};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll, Waker};

fn ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("fixture unexpectedly waited"),
    }
}
fn read_request() -> Request {
    Request::Read {
        device: "fixture".into(),
        service: "180f".into(),
        characteristic: "2a19".into(),
    }
}
fn reading(value: u8) -> Simulation {
    let simulation = Simulation::new();
    simulation.handle(
        |r| matches!(r, Request::Read { .. }),
        move |_, _| Ok(Reply::Value(vec![value])),
    );
    simulation
}

#[test]
fn unchanged_global_api_exercises_a_complete_mock_peripheral() {
    let simulation = Simulation::new();
    let state = Arc::new(Mutex::new((42u8, false)));
    let fixture = state.clone();
    simulation.handle(
        |_| true,
        move |request, events| {
            match request {
                Request::RequestAccess => events.emit(ble::Event::State {
                    state: ble::AdapterState::PoweredOn,
                }),
                Request::StartScan(_) => events.emit(ble::Event::Advertisement {
                    advertisement: ble::Advertisement {
                        device: "fixture".into(),
                        name: Some("Synthetic battery".into()),
                        rssi: -40,
                        services: vec!["180f".into()],
                        manufacturer_data: vec![],
                        connectable: Some(true),
                    },
                }),
                Request::Connect { device } => events.emit(ble::Event::Connected { device }),
                Request::Disconnect { device } => events.emit(ble::Event::Disconnected { device }),
                Request::Services { .. } => {
                    return Ok(Reply::Services(vec![ble::Service {
                        uuid: "180f".into(),
                        characteristics: vec![ble::Characteristic {
                            uuid: "2a19".into(),
                            read: true,
                            write: true,
                            write_without_response: false,
                            notify: true,
                            indicate: false,
                        }],
                    }]));
                }
                Request::Read { .. } => return Ok(Reply::Value(vec![fixture.lock().unwrap().0])),
                Request::Write {
                    device,
                    service,
                    characteristic,
                    value,
                    ..
                } => {
                    let notify = {
                        let mut state = fixture.lock().unwrap();
                        state.0 = value[0];
                        state.1
                    };
                    if notify {
                        events.emit(ble::Event::Value {
                            device,
                            service,
                            characteristic,
                            value,
                        });
                    }
                }
                Request::SetNotify { enabled, .. } => fixture.lock().unwrap().1 = enabled,
                Request::StopScan => {}
            }
            Ok(Reply::Unit)
        },
    );
    let guard = simulation.install();
    assert!(ble::supported());
    let events = Arc::new(Mutex::new(Vec::new()));
    let delivered = events.clone();
    let watch = ble::watch(move |event| delivered.lock().unwrap().push(event)).unwrap();
    ready(async {
        ble::request_access().await?;
        ble::start_scan(&ble::ScanOptions::default()).await?;
        ble::stop_scan().await?;
        ble::connect("fixture").await?;
        assert_eq!(
            ble::services("fixture").await?[0].characteristics[0].uuid,
            "2a19"
        );
        assert_eq!(ble::read("fixture", "180f", "2a19").await?, vec![42]);
        ble::set_notify("fixture", "180f", "2a19", true).await?;
        ble::write("fixture", "180f", "2a19", &[77], true).await?;
        assert_eq!(ble::read("fixture", "180f", "2a19").await?, vec![77]);
        ble::set_notify("fixture", "180f", "2a19", false).await?;
        ble::disconnect("fixture").await
    })
    .unwrap();
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e,ble::Event::Value{value,..} if value==&vec![77]))
    );
    assert_eq!(simulation.request_count(), 11);
    simulation.assert_finished().unwrap();
    // Removing the routing override leaves an existing watch attached to its original source.
    drop(guard);
    simulation.emit(ble::Event::State {
        state: ble::AdapterState::PoweredOff,
    });
    let count = events.lock().unwrap().len();
    drop(watch);
    simulation.emit(ble::Event::State {
        state: ble::AdapterState::PoweredOn,
    });
    assert_eq!(events.lock().unwrap().len(), count);
}

#[test]
fn independent_sessions_can_run_in_parallel_without_global_interference() {
    let threads: Vec<_> = (1..=4)
        .map(|value| {
            std::thread::spawn(move || {
                let session = ble::Session::new();
                let simulation = Simulation::new();
                simulation.expect(read_request(), Ok(Reply::Value(vec![value])));
                session.set_simulation(Some(simulation.clone()));
                assert_eq!(
                    ready(session.read("fixture", "180f", "2a19")).unwrap(),
                    vec![value]
                );
                simulation.assert_finished().unwrap();
                assert_eq!(simulation.requests(), vec![read_request()]);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn strict_mock_mismatches_never_fall_through_and_do_not_consume_expectations() {
    let session = ble::Session::new();
    let simulation = Simulation::new();
    simulation.expect(
        Request::RequestAccess,
        Err(ble::Error {
            kind: ble::ErrorKind::PermissionDenied,
            message: "synthetic permission denial".into(),
        }),
    );
    session.set_simulation(Some(simulation.clone()));
    assert_eq!(
        ready(session.connect("not-a-real-device"))
            .unwrap_err()
            .kind,
        ble::ErrorKind::InvalidArgument
    );
    assert_eq!(
        ready(session.request_access()).unwrap_err().kind,
        ble::ErrorKind::PermissionDenied
    );
    assert!(simulation.assert_finished().is_err());
    assert_eq!(
        ready(session.read("fixture", "180f", "2a19"))
            .unwrap_err()
            .kind,
        ble::ErrorKind::InvalidArgument
    );
}

#[test]
fn out_of_order_guard_drop_restores_the_correct_base_selection() {
    let session = ble::Session::new();
    session.set_simulation(Some(reading(0)));
    let first = session.install_simulation(reading(1));
    let second = session.install_simulation(reading(2));
    drop(first);
    assert_eq!(
        ready(session.read("fixture", "180f", "2a19")).unwrap(),
        vec![2]
    );
    drop(second);
    assert_eq!(
        ready(session.read("fixture", "180f", "2a19")).unwrap(),
        vec![0]
    );
}

#[derive(Default)]
struct Gate {
    released: bool,
    waker: Option<Waker>,
}
struct Delayed {
    gate: Arc<Mutex<Gate>>,
    dropped: Arc<AtomicUsize>,
}
impl Future for Delayed {
    type Output = Result<Reply, ble::Error>;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut gate = self.gate.lock().unwrap();
        if gate.released {
            Poll::Ready(Ok(Reply::Value(vec![13])))
        } else {
            gate.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}
impl Drop for Delayed {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn in_flight_requests_keep_their_handler_and_cancellation_drops_pending_work() {
    let session = ble::Session::new();
    let simulation = Simulation::new();
    let gate = Arc::new(Mutex::new(Gate::default()));
    let dropped = Arc::new(AtomicUsize::new(0));
    let response_gate = gate.clone();
    let response_dropped = dropped.clone();
    simulation.handle_async(
        |_| true,
        move |_, _| Delayed {
            gate: response_gate.clone(),
            dropped: response_dropped.clone(),
        },
    );
    session.set_simulation(Some(simulation.clone()));
    let mut pending = Box::pin(session.read("fixture", "180f", "2a19"));
    let mut context = Context::from_waker(Waker::noop());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    session.set_simulation(Some(reading(99)));
    let wake = {
        let mut state = gate.lock().unwrap();
        state.released = true;
        state.waker.take()
    };
    if let Some(wake) = wake {
        wake.wake();
    }
    assert_eq!(
        pending.as_mut().poll(&mut context),
        Poll::Ready(Ok(vec![13]))
    );
    assert_eq!(
        ready(session.read("fixture", "180f", "2a19")).unwrap(),
        vec![99]
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    gate.lock().unwrap().released = false;
    session.set_simulation(Some(simulation));
    let mut cancelled = Box::pin(session.read("fixture", "180f", "2a19"));
    assert!(cancelled.as_mut().poll(&mut context).is_pending());
    drop(cancelled);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

#[test]
fn invalid_uuids_and_wrong_reply_types_fail_without_native_calls() {
    let session = ble::Session::new();
    let simulation = Simulation::new();
    simulation.handle(|_| true, |_, _| Ok(Reply::Unit));
    session.set_simulation(Some(simulation.clone()));
    assert!(ready(session.read("fixture", "invalid", "2a19")).is_err());
    assert_eq!(simulation.request_count(), 0);
    assert!(ready(session.read("fixture", "180f", "2a19")).is_err());
    assert_eq!(simulation.request_count(), 1);
}

#[test]
fn watcher_can_emit_reentrantly_then_stop_itself() {
    let simulation = Simulation::new();
    let session = ble::Session::new();
    session.set_simulation(Some(simulation.clone()));
    let holder = Arc::new(Mutex::new(None::<ble::Watch>));
    let owner = holder.clone();
    let source = simulation.events();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let watch = session
        .watch(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                source.emit(ble::Event::State {
                    state: ble::AdapterState::PoweredOff,
                });
            } else {
                drop(owner.lock().unwrap().take());
            }
        })
        .unwrap();
    *holder.lock().unwrap() = Some(watch);
    simulation.emit(ble::Event::State {
        state: ble::AdapterState::PoweredOn,
    });
    simulation.emit(ble::Event::State {
        state: ble::AdapterState::Unknown,
    });
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn history_is_bounded_while_total_count_is_preserved() {
    let session = ble::Session::new();
    let simulation = Simulation::new();
    simulation.handle(|_| true, |_, _| Ok(Reply::Unit));
    session.set_simulation(Some(simulation.clone()));
    for _ in 0..300 {
        ready(session.request_access()).unwrap();
    }
    assert_eq!(simulation.requests().len(), 256);
    assert_eq!(simulation.request_count(), 300);
}
