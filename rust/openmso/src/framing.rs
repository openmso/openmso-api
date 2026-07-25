// SPDX-License-Identifier: Apache-2.0
//! OCP framing: newline-delimited JSON messages with optional raw binary
//! payloads.
//!
//! A message is one JSON object per LF-terminated line. If the object carries
//! a top-level integer `binlen`, exactly that many raw bytes follow the LF
//! and form the message's binary payload. See docs/protocol.md section 1.

use serde_json::Value;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::sync::Mutex;

fn protocol_error(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Reads and writes OCP messages over a pair of byte streams.
///
/// Writing is locked so multiple threads (e.g. an acquisition worker emitting
/// capture.data while the main loop answers requests) can interleave whole
/// messages safely.
pub struct MessageStream {
    reader: Mutex<BufReader<Box<dyn Read + Send>>>,
    writer: Mutex<BufWriter<Box<dyn Write + Send>>>,
}

impl MessageStream {
    pub fn new(r: Box<dyn Read + Send>, w: Box<dyn Write + Send>) -> Self {
        MessageStream {
            reader: Mutex::new(BufReader::new(r)),
            writer: Mutex::new(BufWriter::new(w)),
        }
    }

    /// Return `(message, payload)`, or `None` on EOF.
    pub fn read_message(&self) -> io::Result<Option<(Value, Option<Vec<u8>>)>> {
        let mut reader = self.reader.lock().unwrap();
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(None);
            }
            if !line.iter().all(|b| b.is_ascii_whitespace()) {
                break;
            }
        }
        let msg: Value = serde_json::from_slice(&line).map_err(|e| {
            protocol_error(format!("bad JSON line: {e}: {:?}",
                                   String::from_utf8_lossy(&line[..line.len().min(200)])))
        })?;
        if !msg.is_object() {
            return Err(protocol_error("message is not an object".into()));
        }
        let payload = match msg.get("binlen") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let n = v.as_u64()
                    .ok_or_else(|| protocol_error(format!("invalid binlen: {v}")))?;
                let mut buf = vec![0u8; n as usize];
                reader.read_exact(&mut buf).map_err(|e| {
                    protocol_error(format!("EOF inside binary payload: {e}"))
                })?;
                Some(buf)
            }
        };
        Ok(Some((msg, payload)))
    }

    pub fn write_message(&self, msg: &Value, payload: Option<&[u8]>) -> io::Result<()> {
        let line = match payload {
            Some(p) => {
                let mut m = msg.clone();
                m.as_object_mut()
                    .expect("OCP messages are JSON objects")
                    .insert("binlen".into(), Value::from(p.len()));
                serde_json::to_vec(&m)?
            }
            None => serde_json::to_vec(msg)?,
        };
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(&line)?;
        writer.write_all(b"\n")?;
        if let Some(p) = payload {
            writer.write_all(p)?;
        }
        writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use std::sync::{Arc, Mutex as StdMutex};

    #[derive(Clone)]
    struct SharedBuf(Arc<StdMutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn roundtrip(msg: &Value, payload: Option<&[u8]>) -> (Value, Option<Vec<u8>>) {
        let buf = SharedBuf(Arc::new(StdMutex::new(Vec::new())));
        let out = MessageStream::new(Box::new(io::empty()), Box::new(buf.clone()));
        out.write_message(msg, payload).unwrap();
        let wire = buf.0.lock().unwrap().clone();
        let stream = MessageStream::new(Box::new(io::Cursor::new(wire)),
                                        Box::new(io::sink()));
        stream.read_message().unwrap().unwrap()
    }

    #[test]
    fn plain_message_roundtrip() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "scan", "params": {}});
        let (out, payload) = roundtrip(&msg, None);
        assert_eq!(out, msg);
        assert!(payload.is_none());
    }

    #[test]
    fn binary_payload_roundtrip() {
        let data = b"\x00\x01binary\nwith\nnewlines\xff";
        let msg = json!({"jsonrpc": "2.0", "method": "capture.data", "params": {"seq": 0}});
        let (out, payload) = roundtrip(&msg, Some(data));
        assert_eq!(out["binlen"], json!(data.len()));
        assert_eq!(payload.as_deref(), Some(&data[..]));
    }

    #[test]
    fn eof_returns_none() {
        let stream = MessageStream::new(Box::new(io::empty()), Box::new(io::sink()));
        assert!(stream.read_message().unwrap().is_none());
    }

    #[test]
    fn bad_json_is_error() {
        let stream = MessageStream::new(Box::new(io::Cursor::new(b"{nope\n".to_vec())),
                                        Box::new(io::sink()));
        assert!(stream.read_message().is_err());
    }

    #[test]
    fn blank_lines_skipped() {
        let stream = MessageStream::new(
            Box::new(io::Cursor::new(b"\n  \n{\"a\":1}\n".to_vec())),
            Box::new(io::sink()));
        let (msg, _) = stream.read_message().unwrap().unwrap();
        assert_eq!(msg, json!({"a": 1}));
    }
}
