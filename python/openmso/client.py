# SPDX-License-Identifier: Apache-2.0
"""Frontend-side OCP client: launches (or connects to) a capture server.

A background reader thread resolves request futures and hands notifications
to a user callback. The callback runs on the reader thread — keep it quick
(append to buffers; do heavy processing elsewhere).
"""

import socket
import subprocess
import sys
import threading

from . import PROTOCOL_VERSION, __version__
from .framing import MessageStream, ProtocolError


class CaptureError(Exception):
    def __init__(self, code, message, data=None):
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.data = data


class CaptureClient:
    def __init__(self, stream, proc=None, notification_handler=None):
        self._stream = stream
        self._proc = proc
        self._handler = notification_handler
        self._pending = {}          # id -> {"event": Event, "msg": ...}
        self._plock = threading.Lock()
        self._next_id = 0
        self._eof = threading.Event()
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    # -- constructors -----------------------------------------------------
    @classmethod
    def launch(cls, argv, cwd=None, env=None, notification_handler=None):
        """Spawn a capture-server subprocess speaking OCP on its stdio.

        The server's stderr is inherited so its diagnostics reach the user.
        """
        proc = subprocess.Popen(
            argv, cwd=cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None)
        stream = MessageStream(proc.stdout, proc.stdin)
        return cls(stream, proc=proc, notification_handler=notification_handler)

    @classmethod
    def connect(cls, host, port, notification_handler=None):
        sock = socket.create_connection((host, port))
        f = sock.makefile("rwb")
        return cls(MessageStream(f, f), notification_handler=notification_handler)

    # -- reader thread ----------------------------------------------------
    def _read_loop(self):
        try:
            while True:
                item = self._stream.read_message()
                if item is None:
                    break
                msg, payload = item
                if "id" in msg and "method" not in msg:
                    with self._plock:
                        slot = self._pending.get(msg["id"])
                    if slot is not None:
                        slot["msg"] = msg
                        slot["event"].set()
                elif "method" in msg:
                    if self._handler is not None:
                        try:
                            self._handler(msg["method"], msg.get("params") or {},
                                          payload)
                        except Exception:
                            import traceback
                            traceback.print_exc(file=sys.stderr)
        except (ProtocolError, OSError) as e:
            print(f"omso: capture server stream error: {e}", file=sys.stderr)
        finally:
            self._eof.set()
            # Unblock anyone waiting on a request that will never be answered.
            with self._plock:
                for slot in self._pending.values():
                    slot["event"].set()

    # -- API --------------------------------------------------------------
    def set_notification_handler(self, fn):
        self._handler = fn

    def request(self, method, params=None, timeout=60):
        with self._plock:
            self._next_id += 1
            msg_id = self._next_id
            slot = {"event": threading.Event(), "msg": None}
            self._pending[msg_id] = slot
        self._stream.write_message(
            {"jsonrpc": "2.0", "id": msg_id, "method": method,
             "params": params or {}})
        if not slot["event"].wait(timeout):
            with self._plock:
                self._pending.pop(msg_id, None)
            raise TimeoutError(f"no response to {method!r} within {timeout}s")
        with self._plock:
            self._pending.pop(msg_id, None)
        msg = slot["msg"]
        if msg is None:
            raise CaptureError(-1, "capture server exited before responding")
        if "error" in msg:
            err = msg["error"]
            raise CaptureError(err.get("code", -1), err.get("message", "?"),
                               err.get("data"))
        return msg.get("result", {})

    def initialize(self, client_name="openmso-client"):
        return self.request("initialize", {
            "protocol_version": PROTOCOL_VERSION,
            "client": {"name": client_name, "version": __version__}})

    def wait_closed(self, timeout=None):
        return self._eof.wait(timeout)

    def close(self):
        try:
            self.request("shutdown", timeout=5)
        except Exception:
            pass
        self._stream.close()
        if self._proc is not None:
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
