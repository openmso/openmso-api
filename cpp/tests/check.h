// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <cstdio>
#include <cstdlib>
#include <string>

namespace check {

inline int failures = 0;

inline void report(bool ok, const char *expr, const char *file, int line)
{
    if (!ok) {
        std::fprintf(stderr, "%s:%d: FAIL %s\n", file, line, expr);
        ++failures;
    }
}

inline int summary()
{
    if (failures == 0) {
        std::fprintf(stderr, "ok\n");
        return 0;
    }
    std::fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
}

} // namespace check

#define CHECK(expr) ::check::report((expr), #expr, __FILE__, __LINE__)

#define CHECK_EQ(a, b)                                                        \
    do {                                                                      \
        const auto &lhs_ = (a);                                               \
        const auto &rhs_ = (b);                                               \
        ::check::report(lhs_ == rhs_, #a " == " #b, __FILE__, __LINE__);      \
    } while (0)

#define CHECK_THROWS(expr)                                                    \
    do {                                                                      \
        bool threw_ = false;                                                  \
        try {                                                                 \
            (void)(expr);                                                     \
        } catch (...) {                                                       \
            threw_ = true;                                                    \
        }                                                                     \
        ::check::report(threw_, #expr " throws", __FILE__, __LINE__);         \
    } while (0)
