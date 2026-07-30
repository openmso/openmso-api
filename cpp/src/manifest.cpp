// SPDX-License-Identifier: Apache-2.0

#include "openmso/manifest.h"

#include <google/protobuf/util/json_util.h>

#include <fstream>
#include <sstream>

namespace openmso::manifest {

pb::PluginManifest parse(const std::string &json)
{
    pb::PluginManifest manifest;
    google::protobuf::util::JsonParseOptions options;
    options.ignore_unknown_fields = false;

    const auto status =
        google::protobuf::util::JsonStringToMessage(json, &manifest, options);
    if (!status.ok())
        throw Error::json(std::string(status.message()));
    return manifest;
}

pb::PluginManifest load(const std::string &pluginDir)
{
    const std::string path = pluginDir + "/" + FILENAME;
    std::ifstream in(path);
    if (!in)
        throw Error::io(errno, "open " + path);

    std::ostringstream buf;
    buf << in.rdbuf();
    return parse(buf.str());
}

std::vector<std::string> resolveArgv(const pb::PluginManifest &manifest,
                                     const std::string &pluginDir,
                                     const std::string &python)
{
    std::vector<std::string> argv;
    argv.reserve(manifest.run_size());

    for (const auto &token : manifest.run()) {
        if (token == "{python}") {
            argv.push_back(python);
            continue;
        }
        const bool looksLikePath =
            token.find('/') != std::string::npos ||
            (token.size() >= 3 && token.compare(token.size() - 3, 3, ".py") == 0);
        if (!looksLikePath || token.front() == '/') {
            argv.push_back(token);
            continue;
        }
        argv.push_back(pluginDir + "/" + token);
    }
    return argv;
}

} // namespace openmso::manifest
