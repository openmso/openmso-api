// SPDX-License-Identifier: Apache-2.0
//
// Sample encodings, byte codecs, and the `Hello` negotiation between them.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "openmso/error.h"

namespace openmso::encoding {

/// What this library can decode, in preference order. `PACKED` and `NONE` are
/// mandatory for every implementation, so an intersection is never empty.
std::vector<pb::SampleEncoding> supportedEncodings();
std::vector<pb::Codec> supportedCodecs();

/// Intersect a peer's `accept_` list with ours, keeping our preference order.
/// An empty list from the peer means it only promised the mandatory member.
std::vector<int> negotiateEncodings(const std::vector<int> &accept,
                                    const std::vector<pb::SampleEncoding> &supported);
std::vector<int> negotiateCodecs(const std::vector<int> &accept,
                                 const std::vector<pb::Codec> &supported);

bool accepts(const std::vector<int> &negotiated, pb::SampleEncoding e);

/// Run-length encode packed samples as `(varint run, unitsize bytes)` pairs.
///
/// Worst case — every sample different — is one byte per sample of overhead,
/// which is why chunking happens on the packed side.
std::string encodeTransition(const std::string &packed, std::size_t unitsize);

std::string decodeTransition(const std::string &payload, std::size_t unitsize,
                             std::size_t samples);

/// Undo the codec and the sample encoding of one chunk, yielding packed bytes.
///
/// Returns a pointer into `data` when the chunk is already packed and
/// uncompressed — the common path — and into `scratch` otherwise, so the
/// caller keeps both alive for as long as it holds the result.
struct PackedView {
    const char *data = nullptr;
    std::size_t size = 0;
};
PackedView decodePayload(const pb::CaptureData &data, std::size_t unitsize,
                         std::string &scratch);

} // namespace openmso::encoding
