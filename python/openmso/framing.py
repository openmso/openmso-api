# SPDX-License-Identifier: Apache-2.0
"""OCP framing: newline-delimited JSON messages with optional raw binary payloads.

A message is one JSON object per LF-terminated line. If the object carries a
top-level integer ``binlen``, exactly that many raw bytes follow the LF and
form the message's binary payload. See docs/protocol.md section 1.
"""

import json
import threading


class ProtocolError(Exception):
    """Raised when the byte stream violates OCP framing."""


class MessageStream:
    """Reads and writes OCP messages over a pair of binary file objects.

    Writing is locked so multiple threads (e.g. an acquisition worker emitting
    capture.data while the main loop answers requests) can interleave whole
    messages safely.
    """

    def __init__(self, rfile, wfile):
        self._rfile = rfile
        self._wfile = wfile
        self._wlock = threading.Lock()

    def read_message(self):
        """Return (message_dict, payload_bytes_or_None), or None on EOF."""
        while True:
            line = self._rfile.readline()
            if not line:
                return None
            if line.strip() == b"":
                continue
            break
        try:
            msg = json.loads(line)
        except ValueError as e:
            raise ProtocolError(f"bad JSON line: {e}: {line[:200]!r}")
        if not isinstance(msg, dict):
            raise ProtocolError(f"message is not an object: {line[:200]!r}")
        payload = None
        binlen = msg.get("binlen")
        if binlen is not None:
            if not isinstance(binlen, int) or binlen < 0:
                raise ProtocolError(f"invalid binlen: {binlen!r}")
            payload = self._read_exactly(binlen)
        return msg, payload

    def _read_exactly(self, n):
        chunks = []
        remaining = n
        while remaining:
            chunk = self._rfile.read(remaining)
            if not chunk:
                raise ProtocolError(
                    f"EOF inside binary payload ({n - remaining}/{n} bytes read)")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def write_message(self, msg, payload=None):
        if payload is not None:
            msg = dict(msg, binlen=len(payload))
        data = json.dumps(msg, separators=(",", ":")).encode() + b"\n"
        with self._wlock:
            self._wfile.write(data)
            if payload is not None:
                self._wfile.write(payload)
            self._wfile.flush()

    def close(self):
        for f in (self._rfile, self._wfile):
            try:
                f.close()
            except OSError:
                pass
