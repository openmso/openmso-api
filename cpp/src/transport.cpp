// SPDX-License-Identifier: Apache-2.0

#include "openmso/transport.h"

#include <nng/protocol/pipeline0/pull.h>
#include <nng/protocol/pipeline0/push.h>
#include <nng/protocol/reqrep0/rep.h>
#include <nng/protocol/reqrep0/req.h>

#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>

namespace openmso::transport {

namespace {

constexpr nng_duration SEND_TIMEOUT_MS = 30'000;

int openSocket(nng_socket *s, Protocol protocol)
{
    switch (protocol) {
    case Protocol::Req0:  return nng_req0_open(s);
    case Protocol::Rep0:  return nng_rep0_open(s);
    case Protocol::Push0: return nng_push0_open(s);
    case Protocol::Pull0: return nng_pull0_open(s);
    }
    return NNG_EINVAL;
}

void check(int rv, const std::string &context)
{
    if (rv != 0)
        throw Error::nng(rv, context);
}

} // namespace

Socket::Socket(Protocol protocol)
{
    check(openSocket(&socket_, protocol), "open");
    open_ = true;

    try {
        check(nng_socket_set_size(socket_, NNG_OPT_RECVMAXSZ, RECV_MAX_SIZE),
              "set recv-size-max");
        check(nng_socket_set_ms(socket_, NNG_OPT_SENDTIMEO, SEND_TIMEOUT_MS),
              "set send-timeout");
        if (protocol == Protocol::Req0) {
            // nng re-sends a request whose reply outlasts this timer, and the
            // far end runs it twice. Resend buys nothing on a local socket.
            check(nng_socket_set_ms(socket_, NNG_OPT_REQ_RESENDTIME, 0),
                  "set req:resend-time");
        }
    } catch (...) {
        close();
        throw;
    }
}

Socket::~Socket() { close(); }

Socket::Socket(Socket &&other) noexcept
    : socket_(other.socket_), open_(other.open_)
{
    other.open_ = false;
}

Socket &Socket::operator=(Socket &&other) noexcept
{
    if (this != &other) {
        close();
        socket_ = other.socket_;
        open_ = other.open_;
        other.open_ = false;
    }
    return *this;
}

void Socket::close()
{
    if (open_) {
        nng_close(socket_);
        open_ = false;
    }
}

void Socket::listen(const std::string &url)
{
    // Not nng_listen: IPC permissions must be set before the listener starts.
    nng_listener listener{};
    check(nng_listener_create(&listener, socket_, url.c_str()), "listener create");

#ifndef _WIN32
    if (url.rfind("ipc://", 0) == 0) {
        const int rv =
            nng_listener_set_int(listener, NNG_OPT_IPC_PERMISSIONS, 0600);
        if (rv != 0) {
            nng_listener_close(listener);
            throw Error::nng(rv, "set ipc:permissions");
        }
    }
#endif

    const int rv = nng_listener_start(listener, 0);
    if (rv != 0) {
        nng_listener_close(listener);
        throw Error::nng(rv, "listen on " + url);
    }
}

void Socket::dial(const std::string &url)
{
    check(nng_dial(socket_, url.c_str(), nullptr, 0), "dial " + url);
}

void Socket::setRecvTimeout(std::optional<std::chrono::milliseconds> timeout)
{
    const nng_duration d =
        timeout ? static_cast<nng_duration>(timeout->count()) : NNG_DURATION_INFINITE;
    check(nng_socket_set_ms(socket_, NNG_OPT_RECVTIMEO, d), "set recv-timeout");
}

void Socket::send(const google::protobuf::MessageLite &message)
{
    std::string buf;
    if (!message.SerializeToString(&buf))
        throw Error::protocol("could not serialize " + message.GetTypeName());

    check(nng_send(socket_, buf.data(), buf.size(), 0), "send");
}

void Socket::recv(google::protobuf::MessageLite &out)
{
    void *data = nullptr;
    std::size_t size = 0;
    check(nng_recv(socket_, &data, &size, NNG_FLAG_ALLOC), "recv");

    const bool ok = out.ParseFromArray(data, static_cast<int>(size));
    nng_free(data, size);
    if (!ok)
        throw Error::decode("could not parse " + out.GetTypeName());
}

Endpoints::Endpoints()
{
    // Four hex digits only: sun_path is short and hard-limited.
    struct timespec ts {};
    clock_gettime(CLOCK_REALTIME, &ts);
    unsigned tag = (static_cast<unsigned>(ts.tv_nsec) ^
                    static_cast<unsigned>(::getpid())) & 0xffffu;

    const char *runtime = std::getenv("XDG_RUNTIME_DIR");
    const char *tmp = std::getenv("TMPDIR");
    std::string base = runtime ? runtime : (tmp ? tmp : "/tmp");
    base += "/openmso";

    if (::mkdir(base.c_str(), 0700) != 0 && errno != EEXIST)
        throw Error::io(errno, "create " + base);
    // A pre-existing directory keeps its old mode, and it holds every running
    // plugin's sockets.
    if (::chmod(base.c_str(), 0700) != 0)
        throw Error::io(errno, "chmod " + base);

    for (int attempt = 0; attempt < 64; ++attempt) {
        char name[8];
        std::snprintf(name, sizeof(name), "%04x", tag);
        const std::string dir = base + "/" + name;

        if (::mkdir(dir.c_str(), 0700) != 0) {
            if (errno == EEXIST) {
                tag = (tag + 1) & 0xffffu;
                continue;
            }
            throw Error::io(errno, "create " + dir);
        }

        const std::string control = dir + "/ctl";
        try {
            checkSunPath(control);
        } catch (...) {
            ::rmdir(dir.c_str());
            throw;
        }

        dir_ = dir;
        control_ = "ipc://" + control;
        events_ = "ipc://" + dir + "/evt";
        return;
    }

    throw Error::protocol("no free runtime directory");
}

Endpoints::~Endpoints()
{
    if (dir_.empty())
        return;
    ::unlink((dir_ + "/ctl").c_str());
    ::unlink((dir_ + "/evt").c_str());
    ::rmdir(dir_.c_str());
}

void checkSunPath(const std::string &path)
{
    constexpr std::size_t LIMIT = 103;
    if (path.size() > LIMIT) {
        throw Error::protocol("socket path is " + std::to_string(path.size()) +
                              " bytes, over the " + std::to_string(LIMIT) +
                              "-byte limit: " + path);
    }
}

} // namespace openmso::transport
