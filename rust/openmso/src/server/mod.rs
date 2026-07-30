// SPDX-License-Identifier: Apache-2.0
//! Plugin side of OCP: the control loop, the state machine, and the event push.
//!
//! Implement [`CaptureServer`] and hand it to [`serve`]. Handlers run on the
//! control loop in arrival order, so they must return promptly — acquisition
//! belongs on its own thread, pushing through the [`Events`] handle it was
//! given. A plugin that acquires inline cannot answer `AcquireStop`.

mod args;
mod events;

pub use args::{Args, USAGE};
pub use events::{Events, StreamSender};

use std::io::Read;
use std::sync::Arc;

use nng::Protocol;

use crate::encoding::ENCODINGS;
use crate::proto::{
    request, response, AcquireStart, AcquireStop, Config, Description, Empty, Hello, HelloResult,
    Request, Response,
};
use crate::transport;
use crate::{proto, Error, Reply, Result};

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

    let events = Arc::new(Events::new(stream));
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
            events.set_running(true);
            if let Err(e) = plugin.acquire_start(&req, events) {
                events.set_running(false);
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
            events.set_running(false);
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
