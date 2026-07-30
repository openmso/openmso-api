// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <omso/capture/v1/omso.pb.h>

#include <stdexcept>
#include <string>

namespace openmso {

namespace pb = ::omso::capture::v1;

inline constexpr std::uint32_t PROTOCOL_VERSION = 1;

/// `Remote` is a well-formed refusal from the far end; the rest are local.
enum class ErrorKind { Nng, Decode, Io, Json, Protocol, Remote };

class Error : public std::runtime_error {
public:
    static Error nng(int rv, const std::string &context);
    static Error decode(const std::string &what);
    static Error io(int errnoValue, const std::string &context);
    static Error json(const std::string &what);
    static Error protocol(const std::string &what);
    static Error remote(pb::Error error);

    ErrorKind kind() const { return kind_; }

    /// Default-constructed unless kind() is Remote.
    const pb::Error &error() const { return error_; }

private:
    Error(ErrorKind kind, const std::string &what, pb::Error error);

    ErrorKind kind_;
    pb::Error error_;
};

pb::Error makeError(pb::ErrorCode code, const std::string &message);
pb::Error invalidRequest(const std::string &message);
pb::Error unsupported(const std::string &message);
pb::Error busy(const std::string &message);
pb::Error deviceError(const std::string &message);

} // namespace openmso
