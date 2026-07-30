// SPDX-License-Identifier: Apache-2.0

#include "openmso/error.h"

#include <nng/nng.h>

#include <cstring>

namespace openmso {

Error::Error(ErrorKind kind, const std::string &what, pb::Error error)
    : std::runtime_error(what), kind_(kind), error_(std::move(error)) {}

Error Error::nng(int rv, const std::string &context)
{
    return Error(ErrorKind::Nng, "nng: " + context + ": " + nng_strerror(rv),
                 pb::Error());
}

Error Error::decode(const std::string &what)
{
    return Error(ErrorKind::Decode, "malformed message: " + what, pb::Error());
}

Error Error::io(int errno_value, const std::string &context)
{
    return Error(ErrorKind::Io, context + ": " + std::strerror(errno_value),
                 pb::Error());
}

Error Error::json(const std::string &what)
{
    return Error(ErrorKind::Json, "bad manifest JSON: " + what, pb::Error());
}

Error Error::protocol(const std::string &what)
{
    return Error(ErrorKind::Protocol, "protocol error: " + what, pb::Error());
}

Error Error::remote(pb::Error error)
{
    const std::string what =
        pb::ErrorCode_Name(error.code()) + ": " + error.message();
    return Error(ErrorKind::Remote, what, std::move(error));
}

pb::Error makeError(pb::ErrorCode code, const std::string &message)
{
    pb::Error e;
    e.set_code(code);
    e.set_message(message);
    return e;
}

pb::Error invalidRequest(const std::string &message)
{
    return makeError(pb::ERROR_INVALID_REQUEST, message);
}

pb::Error unsupported(const std::string &message)
{
    return makeError(pb::ERROR_UNSUPPORTED, message);
}

pb::Error busy(const std::string &message)
{
    return makeError(pb::ERROR_BUSY, message);
}

pb::Error deviceError(const std::string &message)
{
    return makeError(pb::ERROR_DEVICE, message);
}

} // namespace openmso
