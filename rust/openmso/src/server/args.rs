// SPDX-License-Identifier: Apache-2.0
//! The argv a frontend launches a plugin with.

use crate::{Error, Result};

pub const USAGE: &str = "--device URL --control URL --events URL";

#[derive(Debug, Clone)]
pub struct Args {
    /// URL of the device to drive, e.g. `usb://04b4:8613/2.14` or `demo://0`.
    pub device: String,
    pub control: String,
    pub events: String,
}

impl Args {
    pub fn from_env() -> Result<Args> {
        Args::parse(std::env::args().skip(1))
    }

    /// Accepts both `--flag value` and `--flag=value`.
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
                    return Err(Error::Protocol(format!(
                        "unknown argument {other:?}; usage: {USAGE}"
                    )))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_flag_notations() {
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
    fn incomplete_argv_is_refused() {
        assert!(Args::parse(["--device".to_string(), "demo://0".to_string()]).is_err());
        assert!(Args::parse(["--device".to_string()]).is_err());
        assert!(Args::parse(["--wat=1".to_string()]).is_err());
    }
}
