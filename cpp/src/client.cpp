// SPDX-License-Identifier: Apache-2.0

#include "openmso/client.h"

#include <fcntl.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>
#include <vector>

#include "openmso/encoding.h"

namespace openmso {

namespace {

const char *responseName(const pb::Response &r)
{
    switch (r.response_case()) {
    case pb::Response::kHello:        return "Hello";
    case pb::Response::kDescribe:     return "Describe";
    case pb::Response::kGetConfig:    return "GetConfig";
    case pb::Response::kSetConfig:    return "SetConfig";
    case pb::Response::kAcquireStart: return "AcquireStart";
    case pb::Response::kAcquireStop:  return "AcquireStop";
    case pb::Response::kReset:        return "Reset";
    case pb::Response::kShutdown:     return "Shutdown";
    case pb::Response::RESPONSE_NOT_SET: break;
    }
    return "nothing";
}

/// Every request has exactly one legal reply arm, so anything else is a
/// plugin bug worth naming.
Error unexpected(const char *request, const pb::Response &got)
{
    return Error::protocol(std::string(request) + " answered with a " +
                           responseName(got) + " result");
}

struct Spawned {
    int pid = -1;
    int stdinFd = -1;
};

Spawned spawnPlugin(const std::vector<std::string> &argv)
{
    int stdinPipe[2];
    if (::pipe(stdinPipe) != 0)
        throw Error::io(errno, "pipe");

    // Closed by a successful execvp, so EOF here means the child is running
    // and anything read is the errno that stopped it.
    int errPipe[2];
    if (::pipe(errPipe) != 0) {
        ::close(stdinPipe[0]);
        ::close(stdinPipe[1]);
        throw Error::io(errno, "pipe");
    }
    ::fcntl(errPipe[1], F_SETFD, FD_CLOEXEC);

    std::vector<char *> cargv;
    cargv.reserve(argv.size() + 1);
    for (const auto &a : argv)
        cargv.push_back(const_cast<char *>(a.c_str()));
    cargv.push_back(nullptr);

    const int pid = ::fork();
    if (pid < 0) {
        const int e = errno;
        ::close(stdinPipe[0]);
        ::close(stdinPipe[1]);
        ::close(errPipe[0]);
        ::close(errPipe[1]);
        throw Error::io(e, "fork");
    }

    if (pid == 0) {
        ::close(stdinPipe[1]);
        ::close(errPipe[0]);
        ::dup2(stdinPipe[0], STDIN_FILENO);
        ::close(stdinPipe[0]);
        ::execvp(cargv[0], cargv.data());
        const int e = errno;
        const ssize_t ignored = ::write(errPipe[1], &e, sizeof(e));
        static_cast<void>(ignored);
        ::_exit(127);
    }

    ::close(stdinPipe[0]);
    ::close(errPipe[1]);

    int childErrno = 0;
    const ssize_t n = ::read(errPipe[0], &childErrno, sizeof(childErrno));
    ::close(errPipe[0]);
    if (n == static_cast<ssize_t>(sizeof(childErrno))) {
        int status = 0;
        ::waitpid(pid, &status, 0);
        ::close(stdinPipe[1]);
        throw Error::io(childErrno, "exec " + argv.front());
    }

    return Spawned{pid, stdinPipe[1]};
}

} // namespace

pb::Event EventStream::next()
{
    pb::Event event;
    void *data = nullptr;
    std::size_t size = 0;
    const int rv = nng_recv(socket_, &data, &size, NNG_FLAG_ALLOC);
    if (rv != 0)
        throw Error::nng(rv, "recv event");
    const bool ok = event.ParseFromArray(data, static_cast<int>(size));
    nng_free(data, size);
    if (!ok)
        throw Error::decode("could not parse Event");
    return event;
}

void EventStream::setTimeout(std::optional<std::chrono::milliseconds> timeout)
{
    const nng_duration d =
        timeout ? static_cast<nng_duration>(timeout->count()) : NNG_DURATION_INFINITE;
    const int rv = nng_socket_set_ms(socket_, NNG_OPT_RECVTIMEO, d);
    if (rv != 0)
        throw Error::nng(rv, "set recv-timeout");
}

CaptureClient CaptureClient::launch(const std::vector<std::string> &argv,
                                    const std::string &device)
{
    if (argv.empty())
        throw Error::protocol("plugin has an empty argv");

    CaptureClient client;
    client.endpoints_ = std::make_unique<transport::Endpoints>();

    client.control_ = transport::Socket(transport::Protocol::Req0);
    client.control_.listen(client.endpoints_->control());
    client.control_.setRecvTimeout(REQUEST_TIMEOUT);

    client.events_ = transport::Socket(transport::Protocol::Pull0);
    client.events_.listen(client.endpoints_->events());

    std::vector<std::string> full = argv;
    full.insert(full.end(), {"--device", device,
                             "--control", client.endpoints_->control(),
                             "--events", client.endpoints_->events()});

    const Spawned spawned = spawnPlugin(full);
    client.pid_ = spawned.pid;
    client.stdinFd_ = spawned.stdinFd;
    return client;
}

CaptureClient::~CaptureClient() { terminate(); }

CaptureClient::CaptureClient(CaptureClient &&other) noexcept
    : endpoints_(std::move(other.endpoints_)),
      control_(std::move(other.control_)),
      events_(std::move(other.events_)),
      pid_(other.pid_),
      stdinFd_(other.stdinFd_),
      seq_(other.seq_),
      captureId_(other.captureId_)
{
    other.pid_ = -1;
    other.stdinFd_ = -1;
}

CaptureClient &CaptureClient::operator=(CaptureClient &&other) noexcept
{
    if (this != &other) {
        terminate();
        endpoints_ = std::move(other.endpoints_);
        control_ = std::move(other.control_);
        events_ = std::move(other.events_);
        pid_ = other.pid_;
        stdinFd_ = other.stdinFd_;
        seq_ = other.seq_;
        captureId_ = other.captureId_;
        other.pid_ = -1;
        other.stdinFd_ = -1;
    }
    return *this;
}

void CaptureClient::terminate()
{
    if (stdinFd_ >= 0) {
        ::close(stdinFd_);
        stdinFd_ = -1;
    }
    if (pid_ > 0) {
        // Kill rather than wait: a plugin that never answered Shutdown is the
        // one that would hang here.
        ::kill(pid_, SIGKILL);
        int status = 0;
        ::waitpid(pid_, &status, 0);
        pid_ = -1;
    }
}

void CaptureClient::setRequestTimeout(std::optional<std::chrono::milliseconds> t)
{
    control_.setRecvTimeout(t);
}

void CaptureClient::setEventTimeout(std::optional<std::chrono::milliseconds> t)
{
    events_.setRecvTimeout(t);
}

pb::Response CaptureClient::request(pb::Request &request)
{
    seq_ += 1;
    const std::uint32_t seq = seq_;
    request.set_seq(seq);
    control_.send(request);

    pb::Response response;
    control_.recv(response);

    if (response.seq() != seq) {
        throw Error::protocol("plugin answered request " + std::to_string(seq) +
                              " with a reply to " + std::to_string(response.seq()));
    }
    if (response.has_error())
        throw Error::remote(response.error());
    if (response.response_case() == pb::Response::RESPONSE_NOT_SET)
        throw Error::protocol("reply carries neither result nor error");
    return response;
}

pb::HelloResult CaptureClient::hello(const std::string &clientName,
                                     const std::string &clientVersion)
{
    pb::Request req;
    auto *hello = req.mutable_hello();
    hello->set_protocol(PROTOCOL_VERSION);
    hello->set_client_name(clientName);
    hello->set_client_version(clientVersion);
    for (auto e : encoding::supportedEncodings())
        hello->add_accept_encodings(e);
    for (auto c : encoding::supportedCodecs())
        hello->add_accept_codecs(c);

    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kHello)
        throw unexpected("Hello", r);
    return r.hello();
}

pb::Description CaptureClient::describe()
{
    pb::Request req;
    req.mutable_describe();
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kDescribe)
        throw unexpected("Describe", r);
    return r.describe();
}

pb::Config CaptureClient::getConfig()
{
    pb::Request req;
    req.mutable_get_config();
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kGetConfig)
        throw unexpected("GetConfig", r);
    return r.get_config();
}

pb::Config CaptureClient::setConfig(const pb::Config &config)
{
    pb::Request req;
    *req.mutable_set_config()->mutable_config() = config;
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kSetConfig)
        throw unexpected("SetConfig", r);
    return r.set_config();
}

void CaptureClient::acquireStart(std::uint64_t captureId, pb::AcquireMode mode)
{
    pb::Request req;
    auto *start = req.mutable_acquire_start();
    start->set_capture_id(captureId);
    start->set_mode(mode);
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kAcquireStart)
        throw unexpected("AcquireStart", r);
}

void CaptureClient::acquireStop(std::uint64_t captureId)
{
    pb::Request req;
    req.mutable_acquire_stop()->set_capture_id(captureId);
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kAcquireStop)
        throw unexpected("AcquireStop", r);
}

void CaptureClient::reset()
{
    pb::Request req;
    req.mutable_reset();
    pb::Response r = request(req);
    if (r.response_case() != pb::Response::kReset)
        throw unexpected("Reset", r);
}

void CaptureClient::shutdown()
{
    if (pid_ <= 0)
        return;

    pb::Request req;
    req.mutable_shutdown();
    try {
        pb::Response r = request(req);
        if (r.response_case() != pb::Response::kShutdown)
            throw unexpected("Shutdown", r);
    } catch (...) {
        terminate();
        throw;
    }

    if (stdinFd_ >= 0) {
        ::close(stdinFd_);
        stdinFd_ = -1;
    }
    int status = 0;
    ::waitpid(pid_, &status, 0);
    pid_ = -1;
}

pb::Event CaptureClient::nextEvent()
{
    pb::Event event;
    events_.recv(event);
    return event;
}

} // namespace openmso
