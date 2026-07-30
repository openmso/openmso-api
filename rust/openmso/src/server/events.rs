// SPDX-License-Identifier: Apache-2.0
//! The event stream: the plugin's half of the PUSH/PULL socket.

use std::sync::atomic::{AtomicBool, Ordering};

use nng::Socket;

use crate::encoding::encode_transition;
use crate::proto::{event, CaptureData, Codec, Event, Log, LogLevel, SampleEncoding, State, Status};
use crate::transport::{self, MAX_PAYLOAD};
use crate::Result;

/// The stream socket, shared with acquisition threads.
pub struct Events {
    socket: Socket,
    running: AtomicBool,
}

impl Events {
    pub(super) fn new(socket: Socket) -> Self {
        Events { socket, running: AtomicBool::new(false) }
    }

    pub fn send(&self, event: event::Event) -> Result<()> {
        // The control loop reads the end of a capture off the event stream
        // rather than guessing when an acquisition thread finished.
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

    pub(super) fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
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
