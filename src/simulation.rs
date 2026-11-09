// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Programmable Bluetooth interception without permissions, radios, native UI, or an executor.
//! Install before creating watches/connections. Unmatched requests always fail; simulation
//! never falls through to real hardware. Async handlers can use an application's manual clock.
use crate::{Error, ErrorKind, Event, ScanOptions, Service, Watch, WatchInner, error};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

/// The same logical operations exposed by the public Bluetooth API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    RequestAccess,
    StartScan(ScanOptions),
    StopScan,
    Connect {
        device: String,
    },
    Disconnect {
        device: String,
    },
    Services {
        device: String,
    },
    Read {
        device: String,
        service: String,
        characteristic: String,
    },
    Write {
        device: String,
        service: String,
        characteristic: String,
        value: Vec<u8>,
        with_response: bool,
    },
    SetNotify {
        device: String,
        service: String,
        characteristic: String,
        enabled: bool,
    },
    MaxWriteLength {
        device: String,
        with_response: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Unit,
    Services(Vec<Service>),
    Value(Vec<u8>),
    /// The answer to [`Request::MaxWriteLength`], in bytes.
    Length(usize),
}
type Answer = Result<Reply, Error>;
type ResponseFuture = Pin<Box<dyn Future<Output = Answer> + Send>>;
type Matcher = dyn Fn(&Request) -> bool + Send + Sync;
type Handler = dyn Fn(Request, Events) -> ResponseFuture + Send + Sync;
#[derive(Clone)]
struct Rule {
    matches: Arc<Matcher>,
    handle: Arc<Handler>,
}
#[derive(Default)]
struct State {
    rules: Vec<Rule>,
    expected: VecDeque<(Request, Answer)>,
    history: VecDeque<Request>,
    count: u64,
    unexpected: u64,
}
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    watchers: Mutex<HashSet<u64>>,
}
static STREAMS: day_bridge::Streams<Event> = day_bridge::Streams::new();
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A notification/event source for handlers. A weak reference avoids cycles when a handler
/// retains it. Emission reaches only watches created against this simulation.
#[derive(Clone)]
pub struct Events(Weak<Inner>);
impl Events {
    /// Delivers synchronously; callbacks may enqueue another event or stop their own watch.
    /// Concurrent/reentrant events are serialized per watcher by Day's stream registry.
    pub fn emit(&self, event: Event) {
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        let watchers: Vec<_> = lock(&inner.watchers).iter().copied().collect();
        for token in watchers {
            STREAMS.emit(token, event.clone());
        }
    }
}
/// Shared rules, scripted replies, event subscriptions, and bounded request history.
#[derive(Clone, Default)]
pub struct Simulation(Arc<Inner>);
impl Simulation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a process-wide override for the existing free functions. Dropping the guard
    /// restores the previous selection, including when nested guards drop out of order.
    /// Parallel tests should use separate `Session`s instead of competing global overrides.
    pub fn install(&self) -> crate::SimulationGuard {
        crate::Session::global().install_simulation(self.clone())
    }

    /// Queue an exact request/reply expectation. Queued expectations take priority over rules.
    /// A mismatch fails without consuming the expectation or touching the radio.
    pub fn expect(&self, request: Request, reply: Answer) {
        lock(&self.0.state).expected.push_back((request, reply));
    }

    /// First matching rule wins. Predicates and handlers run outside internal locks.
    pub fn handle(
        &self,
        matches: impl Fn(&Request) -> bool + Send + Sync + 'static,
        handler: impl Fn(Request, Events) -> Answer + Send + Sync + 'static,
    ) {
        self.handle_async(matches, move |request, events| {
            std::future::ready(handler(request, events))
        });
    }
    /// Returning a pending future permits deterministic delayed replies and cancellation tests.
    /// Dropping the public operation future drops this handler future. Already emitted events
    /// and changes made by a handler are not undone, just as a completed device write cannot be.
    pub fn handle_async<F: Future<Output = Answer> + Send + 'static>(
        &self,
        matches: impl Fn(&Request) -> bool + Send + Sync + 'static,
        handler: impl Fn(Request, Events) -> F + Send + Sync + 'static,
    ) {
        lock(&self.0.state).rules.push(Rule {
            matches: Arc::new(matches),
            handle: Arc::new(move |r, e| Box::pin(handler(r, e))),
        });
    }
    pub fn events(&self) -> Events {
        Events(Arc::downgrade(&self.0))
    }
    pub fn emit(&self, event: Event) {
        self.events().emit(event);
    }
    /// Last 256 requests, including contrived write payloads; use only synthetic data in tests.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.0.state).history.iter().cloned().collect()
    }
    pub fn request_count(&self) -> u64 {
        lock(&self.0.state).count
    }
    /// Fails if scripted expectations remain or any request was unmatched/out of order.
    /// An explicitly scripted error response is not considered an unexpected request.
    pub fn assert_finished(&self) -> Result<(), Error> {
        let state = lock(&self.0.state);
        if state.expected.is_empty() && state.unexpected == 0 {
            Ok(())
        } else {
            Err(error(
                ErrorKind::InvalidArgument,
                format!(
                    "{} unconsumed Bluetooth expectations; {} unexpected requests",
                    state.expected.len(),
                    state.unexpected
                ),
            ))
        }
    }
    pub(crate) fn watch(&self, mut callback: impl FnMut(Event) + Send + 'static) -> Watch {
        let emit = day_bridge::Emit::new(&STREAMS, move |item| {
            if let day_bridge::Item::Value(event) = item {
                callback(event);
            }
        });
        let token = emit.token();
        lock(&self.0.watchers).insert(token);
        Watch(WatchInner::Simulated {
            _subscription: Subscription {
                inner: self.0.clone(),
                token,
            },
        })
    }
    pub(crate) async fn dispatch(&self, request: Request) -> Answer {
        let rules = {
            let mut state = lock(&self.0.state);
            state.count = state.count.saturating_add(1);
            state.history.push_back(request.clone());
            if state.history.len() > 256 {
                state.history.pop_front();
            }
            if let Some((expected, _)) = state.expected.front() {
                if expected == &request {
                    return state.expected.pop_front().expect("front exists").1;
                }
                state.unexpected = state.unexpected.saturating_add(1);
                return Err(error(
                    ErrorKind::InvalidArgument,
                    "Bluetooth request did not match the next scripted expectation",
                ));
            }
            state.rules.clone()
        };
        if let Some(rule) = rules.into_iter().find(|rule| (rule.matches)(&request)) {
            (rule.handle)(request, self.events()).await
        } else {
            let mut state = lock(&self.0.state);
            state.unexpected = state.unexpected.saturating_add(1);
            Err(error(
                ErrorKind::InvalidArgument,
                "no Bluetooth simulation rule matched; native access is disabled",
            ))
        }
    }
}
/// Owned by `Watch`; existing subscriptions survive removal of an interception override.
pub(crate) struct Subscription {
    inner: Arc<Inner>,
    token: u64,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.inner.watchers).remove(&self.token);
        STREAMS.stop(self.token);
    }
}
