// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <nng/nng.h>

#include <chrono>
#include <cstddef>
#include <optional>
#include <string>

#include "openmso/error.h"

namespace openmso::transport {

/// The protocol ceiling is 4 MiB; the headroom absorbs run-length encoding,
/// whose worst case adds a byte per sample.
inline constexpr std::size_t MAX_PAYLOAD = 1u << 20;

inline constexpr std::size_t RECV_MAX_SIZE = 8u << 20;

enum class Protocol { Req0, Rep0, Push0, Pull0 };

class Socket {
public:
    Socket() = default;
    explicit Socket(Protocol protocol);
    ~Socket();

    Socket(Socket &&other) noexcept;
    Socket &operator=(Socket &&other) noexcept;
    Socket(const Socket &) = delete;
    Socket &operator=(const Socket &) = delete;

    bool isOpen() const { return open_; }
    nng_socket raw() const { return socket_; }

    void listen(const std::string &url);
    void dial(const std::string &url);

    /// nullopt blocks forever.
    void setRecvTimeout(std::optional<std::chrono::milliseconds> timeout);

    void send(const google::protobuf::MessageLite &message);
    void recv(google::protobuf::MessageLite &out);

private:
    void close();

    nng_socket socket_{};
    bool open_ = false;
};

/// One plugin process's two sockets, in a directory removed on destruction.
/// Windows has no directory to make: ipc:// there is a flat named pipe.
class Endpoints {
public:
    Endpoints();
    ~Endpoints();

    Endpoints(const Endpoints &) = delete;
    Endpoints &operator=(const Endpoints &) = delete;

    const std::string &control() const { return control_; }
    const std::string &events() const { return events_; }

private:
    std::string control_;
    std::string events_;
    std::string dir_;
};

/// Linux allows 108 bytes of sun_path, macOS 104. Budget against the tighter.
void checkSunPath(const std::string &path);

} // namespace openmso::transport
