// SPDX-License-Identifier: Apache-2.0
//! nng sockets carrying protobuf: REQ/REP for control, PUSH/PULL for events.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nng::options::{protocol::reqrep::ResendTime, Options, RecvMaxSize, RecvTimeout, SendTimeout};
use nng::{Protocol, Socket};

use crate::{Error, Result};

/// Largest `CaptureData.payload` a plugin should emit. The protocol's ceiling
/// is 4 MiB; staying well under it leaves room for run-length encoding, whose
/// worst case expands a chunk by one byte per sample.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// Applied to every socket rather than trusted to the default, per the spike.
pub const RECV_MAX_SIZE: usize = 8 << 20;

/// Build a socket with the options every OCP endpoint needs.
pub fn socket(protocol: Protocol) -> Result<Socket> {
    let s = Socket::new(protocol)?;
    s.set_opt::<RecvMaxSize>(RECV_MAX_SIZE)?;
    s.set_opt::<SendTimeout>(Some(Duration::from_secs(30)))?;
    if protocol == Protocol::Req0 {
        // nng re-sends a request whose reply is slower than this timer, and
        // the far end runs it a second time — measured, not theoretical. On a
        // local socket the pipe either exists or breaks, so resend buys
        // nothing; zero disables it.
        s.set_opt::<ResendTime>(Some(Duration::ZERO))?;
    }
    Ok(s)
}

/// Listen on `url`, restricting an `ipc://` socket to the current user.
///
/// `Socket::listen` starts the listener immediately, and IPC permissions can
/// only be set before it starts, so this goes through a builder.
pub fn listen(socket: &Socket, url: &str) -> Result<nng::Listener> {
    let builder = nng::ListenerBuilder::new(socket, url)?;
    #[cfg(unix)]
    if url.starts_with("ipc://") {
        use nng::options::transport::ipc::Permissions;
        builder.set_opt::<Permissions>(0o600)?;
    }
    builder.start().map_err(|(_, e)| Error::Nng(e))
}

pub fn set_recv_timeout(socket: &Socket, timeout: Option<Duration>) -> Result<()> {
    socket.set_opt::<RecvTimeout>(timeout).map_err(Error::from)
}

pub fn send<M: prost::Message>(socket: &Socket, message: &M) -> Result<()> {
    let mut buf = Vec::with_capacity(message.encoded_len());
    message.encode(&mut buf).expect("Vec never runs out of room");
    socket.send(&buf[..]).map_err(|(_, e)| Error::Nng(e))
}

pub fn recv<M: prost::Message + Default>(socket: &Socket) -> Result<M> {
    let msg = socket.recv()?;
    M::decode(msg.as_slice()).map_err(Error::from)
}

/// A directory holding one plugin process's two sockets, removed on drop.
///
/// The frontend owns this; the plugin only ever sees the two URLs. Windows has
/// no directory to make — `ipc://` there is a named pipe in a flat namespace.
pub struct Endpoints {
    pub control: String,
    pub events: String,
    #[cfg(unix)]
    dir: PathBuf,
}

impl Endpoints {
    pub fn new() -> Result<Self> {
        // Four hex digits of process-and-clock entropy. sun_path is 108 bytes
        // on Linux and 104 on macOS, and a long path is rejected outright, so
        // the random component has to stay short.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut tag = (now ^ std::process::id()) & 0xffff;
        for _ in 0..64 {
            match Self::create(tag) {
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    tag = (tag + 1) & 0xffff;
                }
                other => return other,
            }
        }
        Err(Error::Protocol("no free runtime directory".into()))
    }

    #[cfg(unix)]
    fn create(tag: u32) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("openmso");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&base)?;
        // A pre-existing directory keeps its old mode, and it holds every
        // running plugin's sockets.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;

        let dir = base.join(format!("{tag:04x}"));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;

        let control = dir.join("ctl");
        check_sun_path(&control)?;
        Ok(Endpoints {
            control: format!("ipc://{}", control.display()),
            events: format!("ipc://{}", dir.join("evt").display()),
            dir,
        })
    }

    #[cfg(windows)]
    fn create(tag: u32) -> Result<Self> {
        Ok(Endpoints {
            control: format!("ipc://openmso-{tag:04x}-ctl"),
            events: format!("ipc://openmso-{tag:04x}-evt"),
        })
    }
}

#[cfg(unix)]
impl Drop for Endpoints {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// macOS allows 104 bytes including the terminator; budget against the tighter
/// of the two so a capture that works here works there.
#[cfg(unix)]
fn check_sun_path(path: &Path) -> Result<()> {
    const LIMIT: usize = 103;
    let len = path.as_os_str().len();
    if len > LIMIT {
        return Err(Error::Protocol(format!(
            "socket path is {len} bytes, over the {LIMIT}-byte limit: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_private_and_vanish_with_the_frontend() {
        use std::os::unix::fs::PermissionsExt;

        let dir = {
            let e = Endpoints::new().unwrap();
            assert!(e.control.starts_with("ipc://"));
            assert_ne!(e.control, e.events);
            let mode = std::fs::metadata(&e.dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "runtime dir must not be world-readable");
            e.dir.clone()
        };
        assert!(!dir.exists(), "runtime directory outlived its Endpoints");
    }

    #[test]
    fn concurrent_frontends_get_separate_directories() {
        let (a, b) = (Endpoints::new().unwrap(), Endpoints::new().unwrap());
        assert_ne!(a.control, b.control);
    }

    #[test]
    fn overlong_socket_paths_are_rejected_before_nng_sees_them() {
        let long = PathBuf::from("/run/user/1000").join("x".repeat(100));
        assert!(check_sun_path(&long).is_err());
        assert!(check_sun_path(Path::new("/run/user/1000/openmso/7f3a/ctl")).is_ok());
    }
}
