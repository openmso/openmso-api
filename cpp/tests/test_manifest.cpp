// SPDX-License-Identifier: Apache-2.0

#include "openmso/manifest.h"

#include "check.h"

using namespace openmso;

namespace {

void canonical_json_field_names_round_trip()
{
    const auto m = manifest::parse(R"({
        "name": "generic-fx2",
        "description": "Cypress FX2 logic analyzers",
        "version": "0.0.1",
        "run": ["./generic-fx2"],
        "urlSchemes": ["usb", "fd"],
        "usbIds": [{"id": "04b4:8613", "needsFirmware": true},
                   {"id": "1d50:608c", "note": "openmso"}],
        "palette": {"analog": ["COLOR_YELLOW", "COLOR_MAGENTA"]}})");

    CHECK_EQ(m.name(), std::string("generic-fx2"));
    CHECK_EQ(m.url_schemes_size(), 2);
    CHECK_EQ(m.url_schemes(0), std::string("usb"));
    CHECK_EQ(m.usb_ids_size(), 2);
    CHECK(m.usb_ids(0).needs_firmware());
    CHECK(!m.usb_ids(1).needs_firmware());
    CHECK_EQ(m.palette().analog(0), pb::COLOR_YELLOW);
    CHECK_EQ(m.palette().analog(1), pb::COLOR_MAGENTA);
    CHECK_EQ(m.vendor(), std::string(""));
}

void an_unknown_key_is_rejected_rather_than_ignored()
{
    CHECK_THROWS(manifest::parse(R"({"name": "demo", "runn": ["./demo"]})"));
}

void argv_resolves_against_the_plugin_directory()
{
    pb::PluginManifest m;
    for (const char *token :
         {"{python}", "plugin.py", "--verbose", "/usr/bin/thing", "demo"})
        m.add_run(token);

    const auto argv = manifest::resolveArgv(m, "/plugins/demo", "/usr/bin/python3.13");
    CHECK_EQ(argv, (std::vector<std::string>{"/usr/bin/python3.13",
                                             "/plugins/demo/plugin.py",
                                             "--verbose", "/usr/bin/thing", "demo"}));
}

} // namespace

int main()
{
    canonical_json_field_names_round_trip();
    an_unknown_key_is_rejected_rather_than_ignored();
    argv_resolves_against_the_plugin_directory();
    return check::summary();
}
