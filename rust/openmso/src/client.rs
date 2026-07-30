// SPDX-License-Identifier: Apache-2.0
//! Frontend side of OCP: launch a plugin, drive its control socket, and read
//! its event stream.
//!
//! The frontend listens on both sockets before spawning the plugin, which then
//! dials them, so there is no window in which the plugin can find nothing to
//! connect to.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use nng::{Protocol, Socket};

use crate::encoding::{CODECS, ENCODINGS};
use crate::proto::{
    request, response, AcquireMode, AcquireStart, AcquireStop, Config, Describe, Description,
    Event, GetConfig, Hello, HelloResult, Request, Reset, Response, SetConfig, Shutdown,
};
use crate::transport::{self, Endpoints};
use crate::{Error, Result};

/// Long enough for the slowest thing a plugin does while answering a request,
/// which is an fx2 firmware upload.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct CaptureClient {
    child: Child,
    control: Socket,
    events: Socket,
    /// Kept for its Drop, which removes the socket directory.
    _endpoints: Endpoints,
    seq: u32,
    capture_id: u64,
}

impl CaptureClient {
    /// Spawn `argv`, appending the device URL and the two socket URLs.
    pub fn launch(argv: &[String], device: &str) -> Result<Self> {
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| Error::Protocol("plugin has an empty argv".into()))?;

        let endpoints = Endpoints::new()?;
        let control = transport::socket(Protocol::Req0)?;
        transport::listen(&control, &endpoints.control)?;
        transport::set_recv_timeout(&control, Some(REQUEST_TIMEOUT))?;
        let events = transport::socket(Protocol::Pull0)?;
        transport::listen(&events, &endpoints.events)?;

        let child = Command::new(program)
            .args(rest)
            .args(["--device", device])
            .args(["--control", &endpoints.control])
            .args(["--events", &endpoints.events])
            // Not a channel: the plugin watches it for EOF, which the OS
            // delivers if this process dies, so no plugin is ever orphaned.
            .stdin(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        Ok(CaptureClient { child, control, events, _endpoints: endpoints, seq: 0, capture_id: 0 })
    }

    pub fn set_request_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        transport::set_recv_timeout(&self.control, timeout)
    }

    pub fn set_event_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        transport::set_recv_timeout(&self.events, timeout)
    }

    /// Capture ids are the frontend's to assign, so that an event arriving
    /// before the `AcquireStart` reply still names a capture it knows about.
    pub fn next_capture_id(&mut self) -> u64 {
        self.capture_id += 1;
        self.capture_id
    }

    pub fn hello(&mut self, client_name: &str, client_version: &str) -> Result<HelloResult> {
        let hello = Hello {
            protocol: crate::PROTOCOL_VERSION,
            client_name: client_name.to_string(),
            client_version: client_version.to_string(),
            accept_encodings: ENCODINGS.iter().map(|e| *e as i32).collect(),
            accept_codecs: CODECS.iter().map(|c| *c as i32).collect(),
        };
        match self.request(request::Request::Hello(hello))? {
            response::Response::Hello(r) => Ok(r),
            other => Err(unexpected("Hello", &other)),
        }
    }

    pub fn describe(&mut self) -> Result<Description> {
        match self.request(request::Request::Describe(Describe {}))? {
            response::Response::Describe(r) => Ok(r),
            other => Err(unexpected("Describe", &other)),
        }
    }

    pub fn get_config(&mut self) -> Result<Config> {
        match self.request(request::Request::GetConfig(GetConfig {}))? {
            response::Response::GetConfig(r) => Ok(r),
            other => Err(unexpected("GetConfig", &other)),
        }
    }

    /// Sparse: only the fields present in `config` are applied. The result is
    /// what the device settled on.
    pub fn set_config(&mut self, config: Config) -> Result<Config> {
        let request = SetConfig { config: Some(config) };
        match self.request(request::Request::SetConfig(request))? {
            response::Response::SetConfig(r) => Ok(r),
            other => Err(unexpected("SetConfig", &other)),
        }
    }

    pub fn acquire_start(&mut self, capture_id: u64, mode: AcquireMode) -> Result<()> {
        let request = AcquireStart { capture_id, mode: mode as i32 };
        match self.request(request::Request::AcquireStart(request))? {
            response::Response::AcquireStart(_) => Ok(()),
            other => Err(unexpected("AcquireStart", &other)),
        }
    }

    pub fn acquire_stop(&mut self, capture_id: u64) -> Result<()> {
        match self.request(request::Request::AcquireStop(AcquireStop { capture_id }))? {
            response::Response::AcquireStop(_) => Ok(()),
            other => Err(unexpected("AcquireStop", &other)),
        }
    }

    pub fn reset(&mut self) -> Result<()> {
        match self.request(request::Request::Reset(Reset {}))? {
            response::Response::Reset(_) => Ok(()),
            other => Err(unexpected("Reset", &other)),
        }
    }

    /// Ask the plugin to exit, then reap it.
    pub fn shutdown(&mut self) -> Result<()> {
        let result = match self.request(request::Request::Shutdown(Shutdown {})) {
            Ok(response::Response::Shutdown(_)) => Ok(()),
            Ok(other) => Err(unexpected("Shutdown", &other)),
            Err(e) => Err(e),
        };
        self.child.wait().ok();
        result
    }

    /// Block for the next event. Times out per [`Self::set_event_timeout`].
    pub fn next_event(&self) -> Result<Event> {
        transport::recv(&self.events)
    }

    /// A second handle on the event stream, for frontends that read it on a
    /// thread of their own while the control socket stays with the UI.
    pub fn event_stream(&self) -> EventStream {
        EventStream { socket: self.events.clone() }
    }

    fn request(&mut self, request: request::Request) -> Result<response::Response> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        transport::send(&self.control, &Request { seq, request: Some(request) })?;

        let response: Response = transport::recv(&self.control)?;
        if response.seq != seq {
            return Err(Error::Protocol(format!(
                "plugin answered request {seq} with a reply to {}",
                response.seq
            )));
        }
        match (response.error, response.response) {
            (Some(e), _) => Err(Error::Remote(e)),
            (None, Some(r)) => Ok(r),
            (None, None) => Err(Error::Protocol("reply carries neither result nor error".into())),
        }
    }
}

impl Drop for CaptureClient {
    fn drop(&mut self) {
        // Kill rather than wait: a plugin that never answered Shutdown is the
        // one that would hang here.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

pub struct EventStream {
    socket: Socket,
}

impl EventStream {
    pub fn next_event(&self) -> Result<Event> {
        transport::recv(&self.socket)
    }

    pub fn set_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        transport::set_recv_timeout(&self.socket, timeout)
    }
}

/// Every request has exactly one legal reply arm; anything else is a plugin bug.
fn unexpected(request: &str, got: &response::Response) -> Error {
    let name = match got {
        response::Response::Hello(_) => "Hello",
        response::Response::Describe(_) => "Describe",
        response::Response::GetConfig(_) => "GetConfig",
        response::Response::SetConfig(_) => "SetConfig",
        response::Response::AcquireStart(_) => "AcquireStart",
        response::Response::AcquireStop(_) => "AcquireStop",
        response::Response::Reset(_) => "Reset",
        response::Response::Shutdown(_) => "Shutdown",
    };
    Error::Protocol(format!("{request} answered with a {name} result"))
}
