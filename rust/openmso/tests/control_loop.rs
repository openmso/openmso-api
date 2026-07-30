// SPDX-License-Identifier: Apache-2.0
//! The server's state machine, driven over real sockets by a frontend that
//! speaks `Request`/`Response` by hand rather than through `CaptureClient`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use nng::{Protocol, Socket};
use openmso::encoding::decode_transition;
use openmso::proto::{
    event, request, response, AcquireMode, AcquireStart, AcquireStop, CaptureBegin, CaptureEnd,
    Config, Describe, Description, Empty, ErrorCode, Event, GetConfig, Hello, HelloResult, Request,
    Reset, Response, SetConfig, Shutdown,
};
use openmso::server::{self, Args, CaptureServer, Events, StreamSender};
use openmso::transport::{self, Endpoints};
use openmso::Reply;

const UNITSIZE: usize = 1;
/// Two chunks' worth, so the splitting in `StreamSender` is exercised.
const SAMPLES: usize = (1 << 20) + 5000;

struct Stub {
    stop: Arc<AtomicBool>,
}

impl CaptureServer for Stub {
    fn hello(&mut self, req: &Hello, _events: &Arc<Events>) -> Reply<HelloResult> {
        Ok(server::hello_result(
            req,
            Default::default(),
            Default::default(),
            Default::default(),
        ))
    }

    fn describe(&mut self) -> Reply<Description> {
        Ok(Description::default())
    }

    fn get_config(&mut self) -> Reply<Config> {
        Ok(Config::default())
    }

    fn set_config(&mut self, config: &Config) -> Reply<Config> {
        Ok(config.clone())
    }

    fn acquire_start(&mut self, req: &AcquireStart, events: &Arc<Events>) -> Reply<()> {
        let capture_id = req.capture_id;
        let continuous = req.mode == AcquireMode::AcquireContinuous as i32;
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let events = events.clone();
        thread::spawn(move || {
            events
                .send(event::Event::CaptureBegin(CaptureBegin {
                    capture_id,
                    samplerate: 1e6,
                    streams: vec![],
                }))
                .unwrap();
            let mut sender = StreamSender::new(capture_id, 0, 0, UNITSIZE).transition();
            sender.send(&events, &vec![0xa5u8; SAMPLES]).unwrap();
            while continuous && !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            events
                .send(event::Event::CaptureEnd(CaptureEnd { capture_id, error: None }))
                .unwrap();
        });
        Ok(())
    }

    fn acquire_stop(&mut self, _req: &AcquireStop) -> Reply<()> {
        self.stop.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// The frontend half: listens on both sockets, then runs the plugin's serve
/// loop on a thread in place of spawning a process.
struct Harness {
    control: Socket,
    events: Socket,
    _endpoints: Endpoints,
    seq: u32,
}

impl Harness {
    fn start() -> Harness {
        let endpoints = Endpoints::new().unwrap();
        let control = transport::socket(Protocol::Req0).unwrap();
        transport::listen(&control, &endpoints.control).unwrap();
        transport::set_recv_timeout(&control, Some(Duration::from_secs(10))).unwrap();
        let events = transport::socket(Protocol::Pull0).unwrap();
        transport::listen(&events, &endpoints.events).unwrap();
        transport::set_recv_timeout(&events, Some(Duration::from_secs(10))).unwrap();

        let args = Args {
            device: "demo://0".into(),
            control: endpoints.control.clone(),
            events: endpoints.events.clone(),
        };
        thread::spawn(move || {
            let mut stub = Stub { stop: Arc::new(AtomicBool::new(false)) };
            server::serve(&args, &mut stub).unwrap();
        });
        Harness { control, events, _endpoints: endpoints, seq: 0 }
    }

    fn send(&mut self, request: request::Request) -> Response {
        self.seq += 1;
        transport::send(&self.control, &Request { seq: self.seq, request: Some(request) }).unwrap();
        let response: Response = transport::recv(&self.control).unwrap();
        assert_eq!(response.seq, self.seq, "reply is out of step with the request");
        response
    }

    fn ok(&mut self, request: request::Request) -> response::Response {
        let response = self.send(request);
        assert_eq!(response.error, None, "request was refused");
        response.response.expect("a reply with no error carries a result")
    }

    fn refused(&mut self, request: request::Request) -> ErrorCode {
        let response = self.send(request);
        assert!(response.response.is_none(), "a refusal must not carry a result");
        let error = response.error.expect("expected a refusal");
        ErrorCode::try_from(error.code).unwrap()
    }

    fn hello(&mut self) {
        self.ok(request::Request::Hello(Hello {
            protocol: openmso::PROTOCOL_VERSION,
            client_name: "test".into(),
            ..Default::default()
        }));
    }

    fn send_raw_empty(&mut self) -> Response {
        self.seq += 1;
        transport::send(&self.control, &Request { seq: self.seq, request: None }).unwrap();
        transport::recv(&self.control).unwrap()
    }

    fn next_event(&self) -> event::Event {
        let event: Event = transport::recv(&self.events).unwrap();
        event.event.expect("an event with no arm")
    }

    /// Drain a capture, returning the samples it delivered.
    fn drain_capture(&self) -> Vec<u8> {
        let mut samples = Vec::new();
        let mut expected_seq = 0;
        let mut expected_first = 0;
        loop {
            match self.next_event() {
                event::Event::CaptureBegin(_) => {}
                event::Event::Data(data) => {
                    assert_eq!(data.seq, expected_seq, "chunk sequence must not skip");
                    assert_eq!(data.first_sample, expected_first);
                    assert_eq!(data.decoded_len, data.payload.len() as u64);
                    expected_seq += 1;
                    expected_first += data.sample_count;
                    samples.extend(
                        decode_transition(&data.payload, UNITSIZE, data.sample_count as usize)
                            .unwrap(),
                    );
                }
                event::Event::CaptureEnd(end) => {
                    assert_eq!(end.error, None);
                    return samples;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
    }
}

#[test]
fn nothing_is_answered_before_hello() {
    let mut h = Harness::start();
    assert_eq!(
        h.refused(request::Request::Describe(Describe {})),
        ErrorCode::ErrorInvalidRequest
    );
    h.hello();
    h.ok(request::Request::Describe(Describe {}));
    // The handshake happens once per process.
    assert_eq!(
        h.refused(request::Request::Hello(Hello::default())),
        ErrorCode::ErrorInvalidRequest
    );
}

#[test]
fn a_running_capture_blocks_configuration() {
    let mut h = Harness::start();
    h.hello();
    h.ok(request::Request::AcquireStart(AcquireStart {
        capture_id: 7,
        mode: AcquireMode::AcquireContinuous as i32,
    }));

    // The control loop must answer while the acquisition thread runs.
    for request in [
        request::Request::SetConfig(SetConfig { config: Some(Config::default()) }),
        request::Request::GetConfig(GetConfig {}),
        request::Request::Describe(Describe {}),
        request::Request::AcquireStart(AcquireStart { capture_id: 8, mode: 2 }),
    ] {
        assert_eq!(h.refused(request), ErrorCode::ErrorBusy);
    }

    h.ok(request::Request::AcquireStop(AcquireStop { capture_id: 7 }));
    let samples = h.drain_capture();
    assert_eq!(samples, vec![0xa5u8; SAMPLES]);

    // CaptureEnd on the event stream is what returns the plugin to READY.
    h.ok(request::Request::SetConfig(SetConfig { config: Some(Config::default()) }));
}

#[test]
fn a_capture_that_ends_on_its_own_returns_to_ready() {
    let mut h = Harness::start();
    h.hello();
    h.ok(request::Request::AcquireStart(AcquireStart {
        capture_id: 1,
        mode: AcquireMode::AcquireSingle as i32,
    }));
    h.drain_capture();

    // Stopping an already-finished capture is a race the frontend must win.
    h.ok(request::Request::AcquireStop(AcquireStop { capture_id: 1 }));
    h.ok(request::Request::Describe(Describe {}));
}

#[test]
fn reset_returns_to_ready_from_a_running_capture() {
    let mut h = Harness::start();
    h.hello();
    h.ok(request::Request::AcquireStart(AcquireStart {
        capture_id: 1,
        mode: AcquireMode::AcquireContinuous as i32,
    }));
    h.ok(request::Request::Reset(Reset {}));
    h.ok(request::Request::AcquireStart(AcquireStart {
        capture_id: 2,
        mode: AcquireMode::AcquireSingle as i32,
    }));
}

#[test]
fn a_request_with_no_arm_is_refused() {
    let mut h = Harness::start();
    h.hello();
    let response = h.send_raw_empty();
    assert_eq!(
        ErrorCode::try_from(response.error.unwrap().code).unwrap(),
        ErrorCode::ErrorInvalidRequest
    );
    h.ok(request::Request::Describe(Describe {}));
}

#[test]
fn shutdown_is_answered() {
    let mut h = Harness::start();
    h.hello();
    assert_eq!(
        h.ok(request::Request::Shutdown(Shutdown {})),
        response::Response::Shutdown(Empty {})
    );
}
