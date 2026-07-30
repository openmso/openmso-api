// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <chrono>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "openmso/transport.h"

namespace openmso {

/// Covers the slowest request a plugin answers, an fx2 firmware upload.
inline constexpr std::chrono::milliseconds REQUEST_TIMEOUT{60'000};

/// A borrowed handle on the event socket, for reading events on a thread of
/// its own while the control socket stays with the UI. nng sockets are
/// thread-safe; this must not outlive its CaptureClient.
class EventStream {
public:
    explicit EventStream(nng_socket socket) : socket_(socket) {}

    pb::Event next();
    void setTimeout(std::optional<std::chrono::milliseconds> timeout);

private:
    nng_socket socket_;
};

class CaptureClient {
public:
    /// Spawns argv with the device URL and both socket URLs appended.
    static CaptureClient launch(const std::vector<std::string> &argv,
                                const std::string &device);

    ~CaptureClient();

    CaptureClient(CaptureClient &&) noexcept;
    CaptureClient &operator=(CaptureClient &&) noexcept;
    CaptureClient(const CaptureClient &) = delete;
    CaptureClient &operator=(const CaptureClient &) = delete;

    void setRequestTimeout(std::optional<std::chrono::milliseconds> timeout);
    void setEventTimeout(std::optional<std::chrono::milliseconds> timeout);

    /// Frontend-assigned, so an event arriving before the AcquireStart reply
    /// still names a capture the frontend knows about.
    std::uint64_t nextCaptureId() { return ++captureId_; }

    pb::HelloResult hello(const std::string &clientName,
                          const std::string &clientVersion);
    pb::Description describe();
    pb::Config getConfig();

    /// Sparse: only present fields are applied. The result is what the device
    /// settled on.
    pb::Config setConfig(const pb::Config &config);

    void acquireStart(std::uint64_t captureId, pb::AcquireMode mode);
    void acquireStop(std::uint64_t captureId);
    void reset();

    /// Idempotent.
    void shutdown();

    pb::Event nextEvent();

    EventStream eventStream() const { return EventStream(events_.raw()); }

    bool isRunning() const { return pid_ > 0; }

private:
    CaptureClient() = default;

    pb::Response request(pb::Request &request);
    void terminate();

    std::unique_ptr<transport::Endpoints> endpoints_;
    transport::Socket control_;
    transport::Socket events_;
    int pid_ = -1;
    /// Write end of the plugin's stdin. Never written to: the plugin exits on
    /// EOF, which the OS delivers if this process dies, so none are orphaned.
    int stdinFd_ = -1;
    std::uint32_t seq_ = 0;
    std::uint64_t captureId_ = 0;
};

} // namespace openmso
