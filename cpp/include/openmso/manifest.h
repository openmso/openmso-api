// SPDX-License-Identifier: Apache-2.0
//
// plugin.json — `PluginManifest` in protobuf's canonical JSON mapping.

#pragma once

#include <string>
#include <vector>

#include "openmso/error.h"

namespace openmso::manifest {

inline constexpr const char *FILENAME = "plugin.json";

/// Rejects unknown fields, which is what makes the manifest schema-checked
/// rather than advisory.
pb::PluginManifest parse(const std::string &json);

pb::PluginManifest load(const std::string &pluginDir);

/// Expand `run` into an argv: `{python}` becomes `python`, and anything that
/// looks like a relative path resolves against the plugin's own directory.
///
/// A bare word is left alone, so an installed manifest must say `./plugin`
/// rather than `plugin` to run the binary beside itself rather than whatever
/// is first on `PATH`.
std::vector<std::string> resolveArgv(const pb::PluginManifest &manifest,
                                     const std::string &pluginDir,
                                     const std::string &python);

} // namespace openmso::manifest
