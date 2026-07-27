// SPDX-License-Identifier: Apache-2.0
//! Plugin side of OCP: the control loop, the state machine, and the event push.
//!
//! Implement [`CaptureServer`] and hand it to [`serve`]. Handlers run on the
//! control loop in arrival order, so they must return promptly — acquisition
//! belongs on its own thread, pushing through the [`Events`] handle it was
//! given. A plugin that acquires inline cannot answer `AcquireStop`.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use nng::{Protocol, Socket};

use crate::encoding::{encode_transition, ENCODINGS};
use crate::proto::{
    event, request, response, AcquireStart, AcquireStop, CaptureData, Codec, Config, Description,
    Empty, Event, Hello, HelloResult, Log, LogLevel, Request, Response, SampleEncoding, State,
    Status,
};
use crate::transport::{self, MAX_PAYLOAD};
use crate::{proto, Error, Reply, Result};

/// The three arguments a frontend passes on argv.
#[derive(Debug, Clone)]
pub struct Args {
    /// URL of the device to drive, e.g. `usb://04b4:8613/2.14` or `demo://0`.
    pub device: String,
    pub control: String,
    pub events: String,
}

pub const USAGE: &str = "--device URL --control URL --events URL";

impl Args {
    pub fn from_env() -> Result<Args> {
        Args::parse(std::env::args().skip(1))
    }

    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args> {
        let (mut device, mut control, mut events) = (None, None, None);
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (arg, None),
            };
            let slot = match flag.as_str() {
                "--device" => &mut device,
                "--control" => &mut control,
                "--events" => &mut events,
                other => {
                    return Err(Error::Protocol(format!("unknown argument {other:?}; usage: {USAGE}")))
                }
            };
            *slot = match inline.or_else(|| args.next()) {
                Some(v) => Some(v),
                None => return Err(Error::Protocol(format!("{flag} needs a value"))),
            };
        }
        match (device, control, events) {
            (Some(device), Some(control), Some(events)) => Ok(Args { device, control, events }),
            _ => Err(Error::Protocol(format!("missing arguments; usage: {USAGE}"))),
        }
    }
}

/// The stream socket, shared with acquisition threads.
pub struct Events {
    socket: Socket,
    running: AtomicBool,
}

impl Events {
    pub fn send(&self, event: event::Event) -> Result<()> {
        // The capture is over when the plugin says it is, so the control
        // loop's state machine reads it off the event stream rather than
        // guessing at when an acquisition thread finished.
        if matches!(event, event::Event::CaptureEnd(_) | event::Event::DeviceLost(_)) {
            self.running.store(false, Ordering::SeqCst);
        }
        transport::send(&self.socket, &Event { event: Some(event) })
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) -> Result<()> {
        self.send(event::Event::Log(Log { level: level as i32, message: message.into() }))
    }

    pub fn status(&self, state: State, detail: impl Into<String>) -> Result<()> {
        self.send(event::Event::Status(Status { state: state as i32, detail: detail.into() }))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// Cuts one stream of one acquisition into `CaptureData` events, keeping `seq`
/// and `first_sample` consistent across calls.
pub struct StreamSender {
    capture_id: u64,
    acquisition: u64,
    stream: u32,
    unitsize: usize,
    encoding: SampleEncoding,
    seq: u64,
    first_sample: u64,
}

impl StreamSender {
    /// `unitsize` is the packed size of one sample: `LogicFormat.unitsize`, or
    /// the width of the `SampleType` for an analog stream.
    pub fn new(capture_id: u64, acquisition: u64, stream: u32, unitsize: usize) -> Self {
        assert!(unitsize > 0, "unitsize must be positive");
        StreamSender {
            capture_id,
            acquisition,
            stream,
            unitsize,
            encoding: SampleEncoding::Packed,
            seq: 0,
            first_sample: 0,
        }
    }

    /// Emit run-length rather than verbatim samples. Only legal if the
    /// frontend accepted `TRANSITION` in the `Hello` negotiation.
    pub fn transition(mut self) -> Self {
        self.encoding = SampleEncoding::Transition;
        self
    }

    /// `packed` is verbatim samples; chunking and encoding happen here.
    pub fn send(&mut self, events: &Events, packed: &[u8]) -> Result<()> {
        let chunk = MAX_PAYLOAD - MAX_PAYLOAD % self.unitsize;
        for part in packed.chunks(chunk) {
            let samples = (part.len() / self.unitsize) as u64;
            let payload = match self.encoding {
                SampleEncoding::Transition => encode_transition(part, self.unitsize).into(),
                _ => bytes::Bytes::copy_from_slice(part),
            };
            events.send(event::Event::Data(CaptureData {
                capture_id: self.capture_id,
                acquisition: self.acquisition,
                stream: self.stream,
                seq: self.seq,
                first_sample: self.first_sample,
                sample_count: samples,
                encoding: self.encoding as i32,
                codec: Codec::None as i32,
                decoded_len: payload.len() as u64,
                payload,
            }))?;
            self.seq += 1;
            self.first_sample += samples;
        }
        Ok(())
    }
}

pub trait CaptureServer {
    /// Connect to the device named by `--device` and report what it turned out
    /// to be. The negotiated encodings and codecs in the result must be an
    /// intersection of `req.accept_*` and what this plugin can produce; see
    /// [`crate::encoding::negotiate_encodings`].
    fn hello(&mut self, req: &Hello, events: &Arc<Events>) -> Reply<HelloResult>;

    fn describe(&mut self) -> Reply<Description>;

    fn get_config(&mut self) -> Reply<Config>;

    /// Apply the fields present in `config` and report what the device settled
    /// on, which may differ where it snapped a value to a legal step.
    fn set_config(&mut self, config: &Config) -> Reply<Config>;

    /// Start acquiring and return; the work belongs on a thread pushing to
    /// `events`.
    fn acquire_start(&mut self, req: &AcquireStart, events: &Arc<Events>) -> Reply<()>;

    /// Ask a running acquisition to wind up. `CaptureEnd` still follows on the
    /// event stream.
    fn acquire_stop(&mut self, req: &AcquireStop) -> Reply<()>;

    /// Abandon any acquisition and discard buffered data.
    fn reset(&mut self) -> Reply<()> {
        Ok(())
    }

    /// Called after the `Shutdown` reply is sent, before the process exits.
    fn shutdown(&mut self) {}
}

/// Dial the frontend's two sockets and serve until `Shutdown` or a dead pipe.
pub fn serve(args: &Args, plugin: &mut dyn CaptureServer) -> Result<()> {
    let control = transport::socket(Protocol::Rep0)?;
    control.dial(&args.control)?;
    let stream = transport::socket(Protocol::Push0)?;
    stream.dial(&args.events)?;

    let events = Arc::new(Events { socket: stream, running: AtomicBool::new(false) });
    let mut greeted = false;
    loop {
        let request: Request = match transport::recv(&control) {
            Ok(r) => r,
            // The frontend is gone; nothing left to serve.
            Err(Error::Nng(e)) if is_closed(e) => return Ok(()),
            Err(Error::Decode(e)) => {
                transport::send(&control, &Response {
                    seq: 0,
                    error: Some(proto::Error::invalid(format!("undecodable request: {e}"))),
                    response: None,
                })?;
                continue;
            }
            Err(e) => return Err(e),
        };

        let seq = request.seq;
        let shutting_down = matches!(request.request, Some(request::Request::Shutdown(_)));
        let outcome = dispatch(plugin, &events, &mut greeted, request);
        let response = match outcome {
            Ok(response) => Response { seq, error: None, response: Some(response) },
            Err(error) => Response { seq, error: Some(error), response: None },
        };
        transport::send(&control, &response)?;

        if shutting_down && response.error.is_none() {
            plugin.shutdown();
            return Ok(());
        }
    }
}

fn dispatch(
    plugin: &mut dyn CaptureServer,
    events: &Arc<Events>,
    greeted: &mut bool,
    request: Request,
) -> Reply<response::Response> {
    let Some(request) = request.request else {
        return Err(proto::Error::invalid("request carries no payload"));
    };

    // Hello is the only thing a plugin will answer before it has a device.
    if !*greeted && !matches!(request, request::Request::Hello(_)) {
        return Err(proto::Error::invalid("Hello must be the first request"));
    }
    let busy = || proto::Error::busy("acquisition in progress");

    match request {
        request::Request::Hello(req) => {
            if *greeted {
                return Err(proto::Error::invalid("Hello has already been answered"));
            }
            let result = plugin.hello(&req, events)?;
            *greeted = true;
            Ok(response::Response::Hello(result))
        }
        request::Request::Describe(_) if events.is_running() => Err(busy()),
        request::Request::Describe(_) => Ok(response::Response::Describe(plugin.describe()?)),
        request::Request::GetConfig(_) if events.is_running() => Err(busy()),
        request::Request::GetConfig(_) => Ok(response::Response::GetConfig(plugin.get_config()?)),
        request::Request::SetConfig(_) if events.is_running() => Err(busy()),
        request::Request::SetConfig(req) => {
            let config = req.config.unwrap_or_default();
            Ok(response::Response::SetConfig(plugin.set_config(&config)?))
        }
        request::Request::AcquireStart(req) => {
            if events.is_running() {
                return Err(busy());
            }
            // Set before starting: a short capture can push CaptureEnd before
            // acquire_start has even returned, and that must not be undone.
            events.running.store(true, Ordering::SeqCst);
            if let Err(e) = plugin.acquire_start(&req, events) {
                events.running.store(false, Ordering::SeqCst);
                return Err(e);
            }
            Ok(response::Response::AcquireStart(Empty {}))
        }
        request::Request::AcquireStop(req) => {
            // Idempotent: a single-shot capture can end on its own between the
            // frontend deciding to stop it and the request arriving.
            if events.is_running() {
                plugin.acquire_stop(&req)?;
            }
            Ok(response::Response::AcquireStop(Empty {}))
        }
        request::Request::Reset(_) => {
            plugin.reset()?;
            events.running.store(false, Ordering::SeqCst);
            Ok(response::Response::Reset(Empty {}))
        }
        request::Request::Shutdown(_) => Ok(response::Response::Shutdown(Empty {})),
    }
}

/// Exit when stdin reaches EOF, which the OS delivers when the frontend dies,
/// so a plugin is never orphaned. Nothing on the protocol travels over stdio.
///
/// Separate from [`serve`] because it ends the process: a plugin calls it from
/// `main`, and a harness that runs a plugin in-process does not.
pub fn exit_on_stdin_eof() {
    std::thread::spawn(|| {
        let mut sink = [0u8; 256];
        loop {
            match std::io::stdin().read(&mut sink) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(_) => {}
            }
        }
    });
}

fn is_closed(e: nng::Error) -> bool {
    matches!(e, nng::Error::Closed | nng::Error::ConnectionReset | nng::Error::ConnectionAborted)
}

/// Convenience for a plugin that supports every encoding this crate implements.
pub fn hello_result(
    req: &Hello,
    plugin: proto::PluginInfo,
    capabilities: proto::Capabilities,
    device: proto::DeviceInfo,
) -> HelloResult {
    HelloResult {
        protocol: crate::PROTOCOL_VERSION,
        plugin: Some(plugin),
        capabilities: Some(capabilities),
        device: Some(device),
        encodings: crate::encoding::negotiate_encodings(&req.accept_encodings, &ENCODINGS),
        codecs: crate::encoding::negotiate_codecs(&req.accept_codecs, &crate::encoding::CODECS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_three_urls_in_either_notation() {
        let args = Args::parse(
            ["--device", "demo://0", "--control", "ipc:///t/c", "--events", "ipc:///t/e"]
                .map(String::from),
        )
        .unwrap();
        assert_eq!(args.device, "demo://0");
        assert_eq!(args.control, "ipc:///t/c");
        assert_eq!(args.events, "ipc:///t/e");

        let args = Args::parse(
            ["--device=demo://0", "--control=ipc:///t/c", "--events=ipc:///t/e"].map(String::from),
        )
        .unwrap();
        assert_eq!(args.device, "demo://0");
    }

    #[test]
    fn incomplete_argv_is_refused_rather_than_defaulted() {
        assert!(Args::parse(["--device".to_string(), "demo://0".to_string()]).is_err());
        assert!(Args::parse(["--device".to_string()]).is_err());
        assert!(Args::parse(["--wat=1".to_string()]).is_err());
    }
}
