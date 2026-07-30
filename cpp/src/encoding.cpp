// SPDX-License-Identifier: Apache-2.0

#include "openmso/encoding.h"

#include <algorithm>

namespace openmso::encoding {

namespace {

void putVarint(std::string &out, std::uint64_t value)
{
    while (value >= 0x80) {
        out.push_back(static_cast<char>(value | 0x80));
        value >>= 7;
    }
    out.push_back(static_cast<char>(value));
}

std::size_t getVarint(const std::string &in, std::size_t pos, std::uint64_t &out)
{
    std::uint64_t value = 0;
    for (std::size_t i = 0; i < 10 && pos + i < in.size(); ++i) {
        const auto byte = static_cast<std::uint8_t>(in[pos + i]);
        value |= static_cast<std::uint64_t>(byte & 0x7f) << (i * 7);
        if ((byte & 0x80) == 0) {
            if (value == 0)
                throw Error::protocol("transition run length of 0");
            out = value;
            return pos + i + 1;
        }
    }
    throw Error::protocol("truncated or oversized varint");
}

std::vector<int> intersect(const std::vector<int> &accept,
                           const std::vector<int> &supported, int mandatory)
{
    if (accept.empty())
        return {mandatory};

    std::vector<int> out;
    for (int s : supported) {
        if (std::find(accept.begin(), accept.end(), s) != accept.end())
            out.push_back(s);
    }
    if (std::find(out.begin(), out.end(), mandatory) == out.end())
        out.push_back(mandatory);
    return out;
}

} // namespace

std::vector<pb::SampleEncoding> supportedEncodings()
{
    return {pb::SAMPLE_ENCODING_TRANSITION, pb::SAMPLE_ENCODING_PACKED};
}

std::vector<pb::Codec> supportedCodecs() { return {pb::CODEC_NONE}; }

std::vector<int> negotiateEncodings(const std::vector<int> &accept,
                                    const std::vector<pb::SampleEncoding> &supported)
{
    std::vector<int> s(supported.begin(), supported.end());
    return intersect(accept, s, pb::SAMPLE_ENCODING_PACKED);
}

std::vector<int> negotiateCodecs(const std::vector<int> &accept,
                                 const std::vector<pb::Codec> &supported)
{
    std::vector<int> s(supported.begin(), supported.end());
    return intersect(accept, s, pb::CODEC_NONE);
}

bool accepts(const std::vector<int> &negotiated, pb::SampleEncoding e)
{
    return std::find(negotiated.begin(), negotiated.end(), static_cast<int>(e)) !=
           negotiated.end();
}

std::string encodeTransition(const std::string &packed, std::size_t unitsize)
{
    if (unitsize == 0)
        throw Error::protocol("unitsize must be positive");

    std::string out;
    const std::size_t samples = packed.size() / unitsize;
    if (samples == 0)
        return out;

    std::size_t runStart = 0;
    std::uint64_t run = 1;
    for (std::size_t i = 1; i < samples; ++i) {
        if (packed.compare(i * unitsize, unitsize, packed, runStart * unitsize,
                           unitsize) == 0) {
            run += 1;
            continue;
        }
        putVarint(out, run);
        out.append(packed, runStart * unitsize, unitsize);
        runStart = i;
        run = 1;
    }
    putVarint(out, run);
    out.append(packed, runStart * unitsize, unitsize);
    return out;
}

std::string decodeTransition(const std::string &payload, std::size_t unitsize,
                             std::size_t samples)
{
    if (unitsize == 0)
        throw Error::protocol("logic stream declared unitsize 0");

    std::string out;
    out.reserve(samples * unitsize);

    std::size_t pos = 0;
    while (pos < payload.size()) {
        std::uint64_t run = 0;
        pos = getVarint(payload, pos, run);
        if (payload.size() - pos < unitsize)
            throw Error::protocol("transition run is missing its value");
        for (std::uint64_t i = 0; i < run; ++i)
            out.append(payload, pos, unitsize);
        pos += unitsize;
    }

    if (out.size() != samples * unitsize) {
        throw Error::protocol("transition payload expands to " +
                              std::to_string(out.size() / unitsize) +
                              " samples, not the " + std::to_string(samples) +
                              " declared");
    }
    return out;
}

PackedView decodePayload(const pb::CaptureData &data, std::size_t unitsize,
                         std::string &scratch)
{
    switch (data.codec()) {
    case pb::CODEC_NONE:
    case pb::CODEC_UNSPECIFIED:
        break;
    default:
        throw Error::protocol(pb::Codec_Name(data.codec()) +
                              " is not decodable here");
    }

    switch (data.encoding()) {
    case pb::SAMPLE_ENCODING_TRANSITION:
        scratch = decodeTransition(data.payload(), unitsize,
                                   static_cast<std::size_t>(data.sample_count()));
        return PackedView{scratch.data(), scratch.size()};
    default:
        return PackedView{data.payload().data(), data.payload().size()};
    }
}

} // namespace openmso::encoding
