// SPDX-License-Identifier: Apache-2.0
//
// The C++ client against the Rust demo plugin: the cross-implementation check
// that the two stacks agree on the wire.

#include "openmso/client.h"
#include "openmso/encoding.h"

#include <map>
#include <string>
#include <vector>

#include "check.h"

using namespace openmso;

namespace {

constexpr std::uint64_t DEPTH = 5000;

std::string pluginPath;

CaptureClient launch(const std::string &device = "demo://0")
{
    auto client = CaptureClient::launch({pluginPath}, device);
    client.setEventTimeout(std::chrono::milliseconds(20'000));
    return client;
}

std::map<std::uint32_t, std::string> drain(
    CaptureClient &client, const std::map<std::uint32_t, std::size_t> &unitsize)
{
    std::map<std::uint32_t, std::string> streams;
    for (;;) {
        const pb::Event event = client.nextEvent();
        if (event.has_data()) {
            const auto &data = event.data();
            auto it = unitsize.find(data.stream());
            const std::size_t unit = it == unitsize.end() ? 1 : it->second;

            std::string scratch;
            const auto packed = encoding::decodePayload(data, unit, scratch);
            CHECK_EQ(packed.size, data.sample_count() * unit);
            streams[data.stream()].append(packed.data, packed.size);
        } else if (event.has_capture_end()) {
            CHECK(!event.capture_end().has_error());
            return streams;
        }
    }
}

void single_capture_matches_describe()
{
    auto client = launch();

    const auto hello = client.hello("session-test", "0");
    CHECK_EQ(hello.protocol(), PROTOCOL_VERSION);
    CHECK_EQ(hello.device().model(), std::string("Demo MSO"));
    CHECK_EQ(hello.plugin().name(), std::string("demo"));
    CHECK(encoding::accepts({hello.encodings().begin(), hello.encodings().end()},
                            pb::SAMPLE_ENCODING_TRANSITION));

    const auto description = client.describe();
    CHECK_EQ(description.channels_size(), 10);
    CHECK_EQ(description.vendor_options_size(), 3);

    pb::Config wanted;
    wanted.mutable_device()->set_sample_depth(DEPTH);
    (*wanted.mutable_vendor())["frequency"] = "2000";
    const auto settled = client.setConfig(wanted);
    CHECK_EQ(settled.device().sample_depth(), DEPTH);
    CHECK_EQ(settled.device().capture_span(), static_cast<double>(DEPTH) / 1e6);
    CHECK_EQ(settled.vendor().at("frequency"), std::string("2000"));

    const std::uint64_t captureId = client.nextCaptureId();
    client.acquireStart(captureId, pb::ACQUIRE_SINGLE);

    pb::CaptureBegin begin;
    for (;;) {
        const pb::Event event = client.nextEvent();
        if (event.has_capture_begin()) {
            begin = event.capture_begin();
            break;
        }
    }
    CHECK_EQ(begin.capture_id(), captureId);
    CHECK_EQ(begin.samplerate(), 1e6);
    CHECK_EQ(begin.streams_size(), 3);

    std::map<std::uint32_t, std::size_t> unitsize;
    for (const auto &s : begin.streams())
        unitsize[s.id()] = s.has_logic() ? s.logic().unitsize() : 1;

    const auto streams = drain(client, unitsize);
    CHECK_EQ(streams.size(), 3u);
    for (const auto &[id, samples] : streams)
        CHECK_EQ(samples.size(), DEPTH);

    const std::string &logic = streams.at(2);
    bool high = false, low = false;
    int counter = 0;
    for (char c : logic) {
        const auto b = static_cast<std::uint8_t>(c);
        high = high || (b & 0x80) != 0;
        low = low || (b & 0x80) == 0;
        counter = std::max(counter, b & 0x7f);
    }
    CHECK(high && low);
    CHECK(counter > 100);

    client.shutdown();
}

void continuous_capture_repeats_until_stopped()
{
    auto client = launch();
    client.hello("session-test", "0");

    pb::Config wanted;
    wanted.mutable_device()->set_sample_depth(DEPTH);
    client.setConfig(wanted);

    const std::uint64_t captureId = client.nextCaptureId();
    client.acquireStart(captureId, pb::ACQUIRE_CONTINUOUS);

    // Wait for the second frame, so it is definitely re-arming rather than
    // running once, then stop it while data is still in flight.
    std::uint64_t acquisitions = 0;
    while (acquisitions < 2) {
        const pb::Event event = client.nextEvent();
        if (event.has_acquisition_begin()) {
            const auto &begin = event.acquisition_begin();
            CHECK_EQ(begin.capture_id(), captureId);
            CHECK_EQ(begin.sample_count(), DEPTH);
            acquisitions = begin.acquisition() + 1;
        }
    }
    client.acquireStop(captureId);

    for (;;) {
        const pb::Event event = client.nextEvent();
        if (event.has_capture_end()) {
            CHECK(!event.capture_end().has_error());
            break;
        }
    }

    client.getConfig();
    client.shutdown();
}

void unsupported_device_url_fails_at_hello()
{
    auto client = launch("usb://04b4:8613");
    try {
        client.hello("session-test", "0");
        CHECK(false);
    } catch (const Error &e) {
        CHECK_EQ(e.kind(), ErrorKind::Remote);
        CHECK_EQ(e.error().code(), pb::ERROR_DEVICE);
    }
}

void missing_plugin_fails_at_launch()
{
    CHECK_THROWS(CaptureClient::launch({"/nonexistent/plugin"}, "demo://0"));
}

} // namespace

int main(int argc, char **argv)
{
    // CMake passes an empty argument when no demo plugin is configured.
    if (argc < 2 || argv[1][0] == '\0') {
        std::fprintf(stderr, "no demo plugin: skipping\n");
        return 77;
    }
    pluginPath = argv[1];

    single_capture_matches_describe();
    continuous_capture_repeats_until_stopped();
    unsupported_device_url_fails_at_hello();
    missing_plugin_fails_at_launch();
    return check::summary();
}
