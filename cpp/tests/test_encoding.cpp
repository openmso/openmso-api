// SPDX-License-Identifier: Apache-2.0

#include "openmso/encoding.h"

#include "check.h"

using namespace openmso;
using namespace openmso::encoding;

namespace {

std::string bytes(std::initializer_list<int> vs)
{
    std::string s;
    for (int v : vs)
        s.push_back(static_cast<char>(v));
    return s;
}

void roundtrip(const std::string &packed, std::size_t unitsize)
{
    const std::string encoded = encodeTransition(packed, unitsize);
    CHECK_EQ(decodeTransition(encoded, unitsize, packed.size() / unitsize), packed);
}

void runs_survive_a_round_trip()
{
    roundtrip("", 1);
    roundtrip(bytes({7}), 1);
    roundtrip(bytes({1, 1, 1, 2, 2, 3}), 1);
    roundtrip(std::string(500, '\xaa'), 1);
    roundtrip(bytes({1, 2, 1, 2, 3, 4, 3, 4}), 2);
}

void an_idle_bus_costs_almost_nothing()
{
    const std::string idle(100'000, '\0');
    CHECK(encodeTransition(idle, 1).size() < 10);
}

void long_runs_use_multi_byte_varints()
{
    CHECK_EQ(encodeTransition(std::string(300, '\x05'), 1), bytes({0xac, 0x02, 5}));
    roundtrip(std::string(100'000, '\x05'), 1);
}

void a_short_count_is_an_error_not_a_truncated_buffer()
{
    const std::string encoded = encodeTransition(bytes({1, 1, 2}), 1);
    CHECK_THROWS(decodeTransition(encoded, 1, 2));
    CHECK_THROWS(decodeTransition(bytes({3}), 1, 3));
    CHECK_THROWS(decodeTransition(bytes({0, 9}), 1, 1));
}

void negotiation_always_leaves_the_mandatory_member()
{
    const int packed = pb::SAMPLE_ENCODING_PACKED;
    const int transition = pb::SAMPLE_ENCODING_TRANSITION;
    const auto ours = supportedEncodings();

    CHECK_EQ(negotiateEncodings({packed}, ours), std::vector<int>{packed});
    // Our preference order wins, not the client's.
    CHECK_EQ(negotiateEncodings({packed, transition}, ours),
             (std::vector<int>{transition, packed}));
    CHECK_EQ(negotiateEncodings({transition}, {pb::SAMPLE_ENCODING_PACKED}),
             std::vector<int>{packed});
    CHECK_EQ(negotiateEncodings({}, ours), std::vector<int>{packed});
    CHECK_EQ(negotiateCodecs({}, supportedCodecs()),
             std::vector<int>{pb::CODEC_NONE});
}

void chunks_decode_by_their_declared_encoding()
{
    const std::string packed = bytes({1, 1, 1, 2});
    pb::CaptureData data;
    data.set_sample_count(4);
    data.set_encoding(pb::SAMPLE_ENCODING_PACKED);
    data.set_codec(pb::CODEC_NONE);
    data.set_payload(packed);

    std::string scratch;
    PackedView view = decodePayload(data, 1, scratch);
    CHECK_EQ(std::string(view.data, view.size), packed);
    CHECK(view.data == data.payload().data());

    data.set_encoding(pb::SAMPLE_ENCODING_TRANSITION);
    data.set_payload(encodeTransition(packed, 1));
    view = decodePayload(data, 1, scratch);
    CHECK_EQ(std::string(view.data, view.size), packed);

    data.set_codec(pb::CODEC_ZSTD);
    CHECK_THROWS(decodePayload(data, 1, scratch));
}

} // namespace

int main()
{
    runs_survive_a_round_trip();
    an_idle_bus_costs_almost_nothing();
    long_runs_use_multi_byte_varints();
    a_short_count_is_an_error_not_a_truncated_buffer();
    negotiation_always_leaves_the_mandatory_member();
    chunks_decode_by_their_declared_encoding();
    return check::summary();
}
