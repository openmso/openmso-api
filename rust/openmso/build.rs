// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../proto/omso/capture/v1")
        .canonicalize()?;
    let files: Vec<PathBuf> = ["capture.proto", "common.proto", "manifest.proto"]
        .iter()
        .map(|f| proto_dir.join(f))
        .collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    let descriptors = PathBuf::from(std::env::var("OUT_DIR")?).join("descriptors.bin");
    prost_build::Config::new()
        .file_descriptor_set_path(&descriptors)
        // Aliases the receive buffer instead of copying it, which is the whole
        // bulk path.
        .bytes([".omso.capture.v1.CaptureData.payload"])
        .compile_protos(&files, &[&proto_dir])?;

    pbjson_build::Builder::new()
        .register_descriptors(&std::fs::read(&descriptors)?)?
        .build(&[".omso.capture.v1"])?;
    Ok(())
}
