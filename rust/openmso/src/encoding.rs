// SPDX-License-Identifier: Apache-2.0
//! Sample encodings, byte codecs, and the `Hello` negotiation between them.

use std::borrow::Cow;

use crate::proto::{CaptureData, Codec, SampleEncoding};
use crate::{Error, Result};

/// What this crate can decode, in preference order. `PACKED` and `NONE` are
/// mandatory for every implementation, so an intersection is never empty.
pub const ENCODINGS: [SampleEncoding; 2] = [SampleEncoding::Transition, SampleEncoding::Packed];
pub const CODECS: [Codec; 1] = [Codec::None];

/// Intersect a peer's `accept_` list with ours, keeping our preference order.
/// An empty list from the peer means it only promised the mandatory member.
fn intersect(accept: &[i32], supported: &[i32], mandatory: i32) -> Vec<i32> {
    if accept.is_empty() {
        return vec![mandatory];
    }
    let mut out: Vec<i32> =
        supported.iter().copied().filter(|s| accept.contains(s)).collect();
    if !out.contains(&mandatory) {
        out.push(mandatory);
    }
    out
}

pub fn negotiate_encodings(accept: &[i32], supported: &[SampleEncoding]) -> Vec<i32> {
    let supported: Vec<i32> = supported.iter().map(|e| *e as i32).collect();
    intersect(accept, &supported, SampleEncoding::Packed as i32)
}

pub fn negotiate_codecs(accept: &[i32], supported: &[Codec]) -> Vec<i32> {
    let supported: Vec<i32> = supported.iter().map(|c| *c as i32).collect();
    intersect(accept, &supported, Codec::None as i32)
}

pub fn accepts(negotiated: &[i32], encoding: SampleEncoding) -> bool {
    negotiated.contains(&(encoding as i32))
}

/// Run-length encode packed samples as `(varint run, unitsize bytes)` pairs.
///
/// Worst case, every sample different, costs one byte per sample; chunking
/// therefore happens on the packed side.
pub fn encode_transition(packed: &[u8], unitsize: usize) -> Vec<u8> {
    assert!(unitsize > 0, "unitsize must be positive");
    let mut out = Vec::new();
    let mut samples = packed.chunks_exact(unitsize);
    let Some(mut current) = samples.next() else {
        return out;
    };
    let mut run: u64 = 1;
    for sample in samples {
        if sample == current {
            run += 1;
            continue;
        }
        put_varint(&mut out, run);
        out.extend_from_slice(current);
        current = sample;
        run = 1;
    }
    put_varint(&mut out, run);
    out.extend_from_slice(current);
    out
}

pub fn decode_transition(payload: &[u8], unitsize: usize, samples: usize) -> Result<Vec<u8>> {
    if unitsize == 0 {
        return Err(Error::Protocol("logic stream declared unitsize 0".into()));
    }
    let mut out = Vec::with_capacity(samples * unitsize);
    let mut rest = payload;
    while !rest.is_empty() {
        let (run, tail) = get_varint(rest)?;
        if tail.len() < unitsize {
            return Err(Error::Protocol("transition run is missing its value".into()));
        }
        let (value, tail) = tail.split_at(unitsize);
        for _ in 0..run {
            out.extend_from_slice(value);
        }
        rest = tail;
    }
    if out.len() != samples * unitsize {
        return Err(Error::Protocol(format!(
            "transition payload expands to {} samples, not the {samples} declared",
            out.len() / unitsize
        )));
    }
    Ok(out)
}

/// Undo the codec and the sample encoding of one chunk, yielding packed bytes.
/// Borrows when the chunk is already packed and uncompressed, which is the
/// common path.
pub fn decode_payload(data: &CaptureData, unitsize: usize) -> Result<Cow<'_, [u8]>> {
    let codec = Codec::try_from(data.codec).unwrap_or(Codec::Unspecified);
    match codec {
        Codec::None | Codec::Unspecified => {}
        other => {
            return Err(Error::Protocol(format!(
                "{} is not decodable here",
                other.as_str_name()
            )))
        }
    }
    let encoding = SampleEncoding::try_from(data.encoding).unwrap_or(SampleEncoding::Unspecified);
    match encoding {
        SampleEncoding::Packed | SampleEncoding::Unspecified => Ok(Cow::Borrowed(&data.payload)),
        SampleEncoding::Transition => Ok(Cow::Owned(decode_transition(
            &data.payload,
            unitsize,
            data.sample_count as usize,
        )?)),
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn get_varint(input: &[u8]) -> Result<(u64, &[u8])> {
    let mut value: u64 = 0;
    for (i, &byte) in input.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            if value == 0 {
                return Err(Error::Protocol("transition run length of 0".into()));
            }
            return Ok((value, &input[i + 1..]));
        }
    }
    Err(Error::Protocol("truncated or oversized varint".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn roundtrip(packed: &[u8], unitsize: usize) {
        let encoded = encode_transition(packed, unitsize);
        let decoded = decode_transition(&encoded, unitsize, packed.len() / unitsize).unwrap();
        assert_eq!(decoded, packed);
    }

    #[test]
    fn runs_round_trip() {
        roundtrip(&[], 1);
        roundtrip(&[7], 1);
        roundtrip(&[1, 1, 1, 2, 2, 3], 1);
        roundtrip(&[0xaa; 500], 1);
        roundtrip(&[1, 2, 1, 2, 3, 4, 3, 4], 2);
    }

    #[test]
    fn idle_bus_collapses_to_one_run() {
        let idle = vec![0u8; 100_000];
        assert!(encode_transition(&idle, 1).len() < 10, "one run, one value");
    }

    #[test]
    fn long_runs_use_multi_byte_varints() {
        // 300 > 127, so the run length needs two bytes plus the value.
        assert_eq!(encode_transition(&[5u8; 300], 1), vec![0xac, 0x02, 5]);
        roundtrip(&[5u8; 100_000], 1);
    }

    #[test]
    fn a_short_count_is_an_error() {
        let encoded = encode_transition(&[1, 1, 2], 1);
        assert!(decode_transition(&encoded, 1, 2).is_err());
        // A value cut off after its run length.
        assert!(decode_transition(&[3], 1, 3).is_err());
        assert!(decode_transition(&[0, 9], 1, 1).is_err(), "zero-length run");
    }

    #[test]
    fn negotiation_keeps_the_mandatory_member() {
        let packed = SampleEncoding::Packed as i32;
        let transition = SampleEncoding::Transition as i32;

        assert_eq!(negotiate_encodings(&[packed], &ENCODINGS), vec![packed]);
        // Our preference order wins, not the client's.
        assert_eq!(
            negotiate_encodings(&[packed, transition], &ENCODINGS),
            vec![transition, packed]
        );
        // A client asking only for what we cannot produce still gets PACKED.
        assert_eq!(
            negotiate_encodings(&[transition], &[SampleEncoding::Packed]),
            vec![packed]
        );
        assert_eq!(negotiate_encodings(&[], &ENCODINGS), vec![packed]);
        assert_eq!(negotiate_codecs(&[], &CODECS), vec![Codec::None as i32]);
    }

    #[test]
    fn chunks_decode_by_their_declared_encoding() {
        let packed = [1u8, 1, 1, 2];
        let mut data = CaptureData {
            sample_count: 4,
            encoding: SampleEncoding::Packed as i32,
            codec: Codec::None as i32,
            payload: Bytes::from_static(&[1, 1, 1, 2]),
            ..Default::default()
        };
        assert!(matches!(decode_payload(&data, 1).unwrap(), Cow::Borrowed(_)));
        assert_eq!(&*decode_payload(&data, 1).unwrap(), &packed);

        data.encoding = SampleEncoding::Transition as i32;
        data.payload = Bytes::from(encode_transition(&packed, 1));
        assert_eq!(&*decode_payload(&data, 1).unwrap(), &packed);

        data.codec = Codec::Zstd as i32;
        assert!(decode_payload(&data, 1).is_err(), "unimplemented codec must not decode");
    }
}
