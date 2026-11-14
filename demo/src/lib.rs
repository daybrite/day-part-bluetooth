// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Bluetooth Lab: scan, pick a device from a native list, inspect its GATT table, and run the
//! software-peripheral round trip. Every string comes from the resource catalog.

use day::prelude::*;
use day_part_bluetooth as ble;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod fixture;

day::resources!();
day::day_start!(options: res::window_options(), root);

/// One characteristic row of the inspected peripheral's GATT table.
#[derive(Clone, PartialEq)]
struct Row {
    service: String,
    characteristic: String,
    properties: String,
}

fn rows_of(services: &[ble::Service]) -> Vec<Row> {
    let mut rows = Vec::new();
    for service in services {
        for c in &service.characteristics {
            let mut properties = Vec::new();
            if c.read {
                properties.push(res::str::prop_read().format());
            }
            if c.write {
                properties.push(res::str::prop_write().format());
            }
            if c.write_without_response {
                properties.push(res::str::prop_write_no_response().format());
            }
            if c.notify {
                properties.push(res::str::prop_notify().format());
            }
            if c.indicate {
                properties.push(res::str::prop_indicate().format());
            }
            rows.push(Row {
                service: service.uuid.clone(),
                characteristic: c.uuid.clone(),
                properties: properties.join(", "),
            });
        }
    }
    rows
}

pub fn root() -> impl Piece {
    let found = Signal::new(Vec::<ble::Advertisement>::new());
    let state = Signal::new(ble::AdapterState::Unknown);
    let status = Signal::new(res::str::ready().format());
    let selected = Signal::new(None::<String>);
    let table = Signal::new(Vec::<Row>::new());
    let values = found.setter();
    let states = state.setter();
    let cache = Arc::new(Mutex::new(BTreeMap::<String, ble::Advertisement>::new()));
    let watch = ble::watch(move |event| match event {
        ble::Event::State { state } => states.set(state),
        ble::Event::Advertisement { advertisement } => {
            let mut map = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    column((
        column((
            label(res::str::app_title())
                .font(Font::Title)
                .id(day::id!(title)),
            label(res::str::intro()).font(Font::Footnote).secondary(),
            label(move || match state.get() {
                ble::AdapterState::PoweredOn => res::str::adapter_ready(),
                ble::AdapterState::PoweredOff => res::str::adapter_off(),
                ble::AdapterState::Unauthorized => res::str::adapter_denied(),
                ble::AdapterState::Unsupported => res::str::adapter_unsupported(),
                ble::AdapterState::Resetting => res::str::adapter_waiting(),
                ble::AdapterState::Unknown => res::str::adapter_unknown(),
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
            label(status).font(Font::Caption).id(day::id!(status)),
            label(move || res::str::device_count(found.get().len().to_string()).format())
                .font(Font::Footnote)
                .bold()
                .id(day::id!(device_count)),
        ))
        .spacing(10.0)
        .align(HAlign::Leading)
        .padding(16.0),
        list(
            items(
                move || found.get(),
                |a: &ble::Advertisement| a.device.clone(),
            ),
            move |slot| {
                column((
                    label(move || {
                        slot.get()
                            .name
                            .unwrap_or_else(|| res::str::unnamed().format())
                    })
                    .font(Font::Callout)
                    .bold()
                    .single_line(),
                    label(move || slot.get().device)
                        .font(Font::Caption)
                        .monospace()
                        .secondary()
                        .single_line(),
                    label(move || {
                        let a = slot.get();
                        let signal = res::str::signal_strength(a.rssi.to_string()).format();
                        if a.services.is_empty() {
                            signal
                        } else {
                            format!("{signal} · {}", a.services.join(", "))
                        }
                    })
                    .font(Font::Caption2)
                    .secondary()
                    .single_line(),
                ))
                .spacing(3.0)
                .align(HAlign::Leading)
                .grow(Dimension::Width)
            },
        )
        // Rows size to their three lines at the platform's own text size and row insets.
        .row_height(RowHeight::Automatic)
        .on_select(move |id| selected.set(Some(id)))
        .selected_rows(move || {
            selected
                .get()
                .and_then(|id| found.get().iter().position(|a| a.device == id))
                .into_iter()
                .collect()
        })
        .a11y(|a| a.label(res::str::devices()))
        .id(day::id!(devices))
        .grow(Dimension::Both),
        scroll(
            column((
                label(move || match selected.get() {
                    Some(id) => res::str::selected_device(id).format(),
                    None => res::str::selected_none().format(),
                })
                .font(Font::Caption)
                .id(day::id!(selected_device)),
                row((
                    button(res::str::inspect())
                        .id(day::id!(inspect))
                        .enabled(move || selected.get().is_some())
                        .action(move || {
                            let Some(id) = selected.get() else { return };
                            day::task(async move {
                                status.set(res::str::working().format());
                                let result = async {
                                    ble::stop_scan().await?;
                                    ble::connect(&id).await?;
                                    ble::services(&id).await
                                }
                                .await;
                                match result {
                                    Ok(list) => {
                                        table.set(rows_of(&list));
                                        status.set(
                                            res::str::inspected(list.len().to_string()).format(),
                                        );
                                    }
                                    Err(e) => {
                                        status
                                            .set(res::str::operation_error(e.to_string()).format());
                                    }
                                }
                            });
                        }),
                    button(res::str::disconnect())
                        .id(day::id!(disconnect))
                        .enabled(move || selected.get().is_some())
                        .action(move || {
                            let Some(id) = selected.get() else { return };
                            day::task(async move {
                                table.set(Vec::new());
                                status.set(match ble::disconnect(&id).await {
                                    Ok(()) => res::str::disconnected().format(),
                                    Err(e) => res::str::operation_error(e.to_string()).format(),
                                });
                            });
                        }),
                ))
                .spacing(8.0),
                label(move || {
                    table
                        .get()
                        .iter()
                        .map(|r| {
                            format!("{}\n  {}\n  {}", r.service, r.characteristic, r.properties)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .font(Font::Caption)
                .monospace()
                .id(day::id!(services)),
                label(res::str::fixture_help())
                    .font(Font::Footnote)
                    .secondary(),
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
            .spacing(10.0)
            .align(HAlign::Leading)
            .padding(16.0),
        )
        .grow(Dimension::Both),
    ))
    .align(HAlign::Leading)
    .grow(Dimension::Both)
}
