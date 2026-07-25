# OpenMSO API

This repository contains minimal reference bindings for the **OpenMSO Capture Protocol (OCP)** - the protocol which OpenMSO uses to obtain digital or analog samples from an out-of-process capture server.

- The capture client is responsible for processing samples and presenting them to the user.
- The capture server is responsible for communicationg with actual test equipment, such as digital/mixed signal oscilloscopes and logic analyzers.

OpenMSO contains [a number of capture servers](https://github.com/openmso/openmso/tree/main/plugins) which use these bindings. 

## License

Licensed under Apache-2.0.

