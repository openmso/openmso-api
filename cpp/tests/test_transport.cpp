// SPDX-License-Identifier: Apache-2.0

#include "openmso/transport.h"

#include <sys/stat.h>

#include <string>

#include "check.h"

using namespace openmso;
using namespace openmso::transport;

namespace {

std::string pathOf(const std::string &url)
{
    return url.substr(std::string("ipc://").size());
}

void endpoints_are_private_and_vanish_with_the_frontend()
{
    std::string dir;
    {
        Endpoints e;
        CHECK(e.control().rfind("ipc://", 0) == 0);
        CHECK(e.control() != e.events());

        dir = pathOf(e.control());
        dir.erase(dir.rfind('/'));

        struct stat st {};
        CHECK_EQ(::stat(dir.c_str(), &st), 0);
        CHECK_EQ(st.st_mode & 0777, 0700u);
    }

    struct stat st {};
    CHECK(::stat(dir.c_str(), &st) != 0);
}

void concurrent_frontends_get_separate_directories()
{
    Endpoints a;
    Endpoints b;
    CHECK(a.control() != b.control());
}

void overlong_socket_paths_are_rejected_before_nng_sees_them()
{
    CHECK_THROWS(checkSunPath("/run/user/1000/" + std::string(100, 'x')));
    checkSunPath("/run/user/1000/openmso/7f3a/ctl");
}

void a_message_round_trips_over_ipc()
{
    Endpoints endpoints;
    Socket pull(Protocol::Pull0);
    pull.listen(endpoints.events());
    pull.setRecvTimeout(std::chrono::milliseconds(5000));

    Socket push(Protocol::Push0);
    push.dial(endpoints.events());

    pb::Event sent;
    sent.mutable_log()->set_level(pb::LOG_WARNING);
    sent.mutable_log()->set_message("hello");
    push.send(sent);

    pb::Event got;
    pull.recv(got);
    CHECK_EQ(got.log().message(), std::string("hello"));
    CHECK_EQ(got.log().level(), pb::LOG_WARNING);
}

} // namespace

int main()
{
    endpoints_are_private_and_vanish_with_the_frontend();
    concurrent_frontends_get_separate_directories();
    overlong_socket_paths_are_rejected_before_nng_sees_them();
    a_message_round_trips_over_ipc();
    return check::summary();
}
