// SPDX-License-Identifier: Apache-2.0
//! `plugin.json` — `PluginManifest` in protobuf's canonical JSON mapping.

use std::path::Path;

use crate::proto::PluginManifest;
use crate::Result;

pub const FILENAME: &str = "plugin.json";

pub fn parse(json: &str) -> Result<PluginManifest> {
    serde_json::from_str(json).map_err(crate::Error::from)
}

pub fn load(plugin_dir: &Path) -> Result<PluginManifest> {
    parse(&std::fs::read_to_string(plugin_dir.join(FILENAME))?)
}

/// Expand `run` into an argv: `{python}` becomes `python`, and anything that
/// looks like a relative path resolves against the plugin's own directory.
///
/// A bare word is left alone, so an installed manifest must say `./plugin`
/// rather than `plugin` to run the binary beside itself rather than whatever
/// is first on `PATH`.
pub fn resolve_argv(manifest: &PluginManifest, plugin_dir: &Path, python: &str) -> Vec<String> {
    manifest
        .run
        .iter()
        .map(|token| {
            if token == "{python}" {
                return python.to_string();
            }
            let looks_like_path = token.contains('/') || token.ends_with(".py");
            let path = Path::new(token);
            if !looks_like_path || path.is_absolute() {
                return token.clone();
            }
            plugin_dir.join(path).to_string_lossy().into_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ChannelColor;

    #[test]
    fn canonical_json_field_names_round_trip() {
        let m = parse(
            r#"{"name": "generic-fx2",
                "description": "Cypress FX2 logic analyzers",
                "version": "0.0.1",
                "run": ["./generic-fx2"],
                "urlSchemes": ["usb", "fd"],
                "usbIds": [{"id": "04b4:8613", "needsFirmware": true},
                           {"id": "1d50:608c", "note": "openmso"}],
                "palette": {"analog": ["COLOR_YELLOW", "COLOR_MAGENTA"]}}"#,
        )
        .unwrap();

        assert_eq!(m.name, "generic-fx2");
        assert_eq!(m.url_schemes, ["usb", "fd"]);
        assert_eq!(m.usb_ids.len(), 2);
        assert!(m.usb_ids[0].needs_firmware);
        assert!(!m.usb_ids[1].needs_firmware);
        assert_eq!(m.palette.unwrap().analog, [
            ChannelColor::ColorYellow as i32,
            ChannelColor::ColorMagenta as i32
        ]);
        // Absent fields are the proto3 defaults, not an error.
        assert_eq!(m.vendor, "");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse(r#"{"name": "demo", "runn": ["./demo"]}"#).is_err());
    }

    #[test]
    fn argv_resolves_against_the_plugin_directory() {
        let dir = Path::new("/plugins/demo");
        let manifest = PluginManifest {
            run: ["{python}", "plugin.py", "--verbose", "/usr/bin/thing", "demo"]
                .map(String::from)
                .to_vec(),
            ..Default::default()
        };
        assert_eq!(
            resolve_argv(&manifest, dir, "/usr/bin/python3.13"),
            ["/usr/bin/python3.13", "/plugins/demo/plugin.py", "--verbose",
             "/usr/bin/thing", "demo"]
        );
    }
}
