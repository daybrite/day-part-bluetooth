use day::prelude::*;
use day_part_bluetooth as ble;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod fixture;

day::resources!();
day::day_start!(options: res::window_options(), root);

pub fn root() -> impl Piece {
    let found = Signal::new(Vec::<ble::Advertisement>::new());
    let state = Signal::new(ble::AdapterState::Unknown);
    let status = Signal::new(res::str::ready().format());
    let device = Signal::new(String::new());
    let services = Signal::new(String::new());
    let values = found.setter();
    let states = state.setter();
    let cache = Arc::new(Mutex::new(BTreeMap::<String, ble::Advertisement>::new()));
    let watch = ble::watch(move |event| match event {
        ble::Event::State { state } => states.set(state),
        ble::Event::Advertisement { advertisement } => {
            let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
            if map.len() < 256 || map.contains_key(&advertisement.device) {
                map.insert(advertisement.device.clone(), advertisement);
                values.set(map.values().cloned().collect());
            }
        }
        _ => {}
    });
    if let Ok(watch) = watch {
        Scope::current().on_cleanup(move || drop(watch));
    }
    scroll(
        column((
            label(res::str::app_title())
                .font(Font::Title)
                .id(day::id!(title)),
            label(res::str::intro()),
            label(move || match state.get() {
                ble::AdapterState::PoweredOn => res::str::adapter_ready(),
                ble::AdapterState::PoweredOff => res::str::adapter_off(),
                ble::AdapterState::Unauthorized => res::str::adapter_denied(),
                ble::AdapterState::Unsupported => res::str::adapter_unsupported(),
                _ => res::str::adapter_waiting(),
            })
            .id(day::id!(adapter_state)),
            row((
                button(res::str::scan()).id(day::id!(scan)).action(move || {
                    day::task(async move {
                        status.set(res::str::working().format());
                        let result = match ble::request_access().await {
                            Ok(()) => ble::start_scan(&ble::ScanOptions::default()).await,
                            Err(e) => Err(e),
                        };
                        status.set(match result {
                            Ok(()) => res::str::scanning().format(),
                            Err(e) => res::str::operation_error(e.to_string()).format(),
                        });
                    });
                }),
                button(res::str::stop()).id(day::id!(stop)).action(move || {
                    day::task(async move {
                        status.set(match ble::stop_scan().await {
                            Ok(()) => res::str::stopped().format(),
                            Err(e) => res::str::operation_error(e.to_string()).format(),
                        });
                    });
                }),
            ))
            .spacing(8.0),
            label(status).id(day::id!(status)),
            label(move || res::str::device_count(found.get().len().to_string()).format())
                .id(day::id!(device_count)),
            label(move || {
                found
                    .get()
                    .iter()
                    .map(|a| {
                        let name = a
                            .name
                            .clone()
                            .unwrap_or_else(|| res::str::unnamed().format());
                        format!(
                            "{}\n{}\n{}\n{}",
                            name,
                            a.device,
                            res::str::signal_strength(a.rssi.to_string()).format(),
                            a.services.join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .id(day::id!(devices)),
            label(res::str::inspect_help()),
            text_field(device)
                .placeholder(res::str::device_hint())
                .a11y(|a| a.label(res::str::device_hint()))
                .id(day::id!(device_id)),
            row((
                button(res::str::inspect())
                    .id(day::id!(inspect))
                    .action(move || {
                        day::task(async move {
                            let id = device.get();
                            status.set(res::str::working().format());
                            let result = async {
                                ble::stop_scan().await?;
                                ble::connect(&id).await?;
                                ble::services(&id).await
                            }
                            .await;
                            match result {
                                Ok(list) => {
                                    services.set(
                                        serde_json::to_string_pretty(&list).unwrap_or_default(),
                                    );
                                    status.set(res::str::inspected().format());
                                }
                                Err(e) => {
                                    status.set(res::str::operation_error(e.to_string()).format())
                                }
                            }
                        });
                    }),
                button(res::str::disconnect())
                    .id(day::id!(disconnect))
                    .action(move || {
                        day::task(async move {
                            status.set(match ble::disconnect(&device.get()).await {
                                Ok(()) => res::str::disconnected().format(),
                                Err(e) => res::str::operation_error(e.to_string()).format(),
                            });
                        });
                    }),
            ))
            .spacing(8.0),
            label(services).id(day::id!(services)),
            label(res::str::fixture_help()),
            button(res::str::fixture_run())
                .id(day::id!(fixture_run))
                .action(move || {
                    day::task(async move {
                        status.set(res::str::working().format());
                        status.set(match fixture::roundtrip().await {
                            Ok(()) => res::str::fixture_passed().format(),
                            Err(error) => res::str::operation_error(error.to_string()).format(),
                        });
                    });
                }),
        ))
        .spacing(12.0)
        .padding(20.0),
    )
}
