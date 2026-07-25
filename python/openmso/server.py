# SPDX-License-Identifier: Apache-2.0
"""Capture-server side of OCP: JSON-RPC dispatch loop over a MessageStream.

Subclass CaptureServer, fill in ``info``/``capabilities`` and implement the
``on_*`` handlers. Handlers run on the main serve loop in request order;
long-running acquisition belongs in a worker thread that emits notifications
via :meth:`notify` (writes are thread-safe).
"""

import socket
import sys
import traceback

from . import PROTOCOL_VERSION
from .framing import MessageStream, ProtocolError

# JSON-RPC error codes
PARSE_ERROR = -32700
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603
# OCP plugin error codes (>= 1000)
DEVICE_ERROR = 1000
DEVICE_DISCONNECTED = 1001
BUSY = 1002
UNSUPPORTED = 1003


class RpcError(Exception):
    def __init__(self, code, message, data=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


class CaptureServer:
    info = {"name": "unnamed", "version": "0", "vendor": "OpenMSO"}
    capabilities = {}

    def __init__(self):
        self._stream = None
        self._shutdown = False

    # -- entry points -----------------------------------------------------
    def run_stdio(self):
        stream = MessageStream(sys.stdin.buffer, sys.stdout.buffer)
        self.serve(stream)

    def run_tcp(self, host, port):
        srv = socket.create_server((host, port))
        self.log("info", f"listening on {host}:{port}")
        conn, addr = srv.accept()
        self.log("info", f"client connected from {addr}")
        with conn:
            f = conn.makefile("rwb")
            self.serve(MessageStream(f, f))

    def run_from_argv(self, argv=None):
        argv = sys.argv[1:] if argv is None else argv
        if "--listen" in argv:
            hostport = argv[argv.index("--listen") + 1]
            host, _, port = hostport.rpartition(":")
            self.run_tcp(host or "127.0.0.1", int(port))
        else:
            self.run_stdio()

    # -- serve loop -------------------------------------------------------
    def serve(self, stream):
        self._stream = stream
        while not self._shutdown:
            try:
                item = stream.read_message()
            except ProtocolError as e:
                print(f"protocol error: {e}", file=sys.stderr)
                break
            if item is None:
                break  # EOF: frontend went away
            msg, payload = item
            self._dispatch(msg, payload)
        self.on_disconnect()

    def _dispatch(self, msg, payload):
        method = msg.get("method")
        msg_id = msg.get("id")
        if method is None:
            return  # responses from client: none expected in v0
        handler = getattr(self, "on_" + method.replace(".", "_"), None)
        if handler is None:
            if msg_id is not None:
                self._reply_error(msg_id, METHOD_NOT_FOUND,
                                  f"method not found: {method}")
            return
        try:
            params = msg.get("params") or {}
            result = handler(params, payload) if payload is not None \
                else handler(params)
            if msg_id is not None:
                self._stream.write_message(
                    {"jsonrpc": "2.0", "id": msg_id, "result": result or {}})
        except RpcError as e:
            if msg_id is not None:
                self._reply_error(msg_id, e.code, e.message, e.data)
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            if msg_id is not None:
                self._reply_error(msg_id, INTERNAL_ERROR,
                                  f"{type(e).__name__}: {e}")

    def _reply_error(self, msg_id, code, message, data=None):
        err = {"code": code, "message": message}
        if data is not None:
            err["data"] = data
        self._stream.write_message({"jsonrpc": "2.0", "id": msg_id, "error": err})

    # -- outgoing ---------------------------------------------------------
    def notify(self, method, params, payload=None):
        self._stream.write_message(
            {"jsonrpc": "2.0", "method": method, "params": params}, payload)

    def log(self, level, message):
        # Before a client is connected, stderr is all we have.
        if self._stream is not None:
            try:
                self.notify("log", {"level": level, "message": message})
                return
            except OSError:
                pass
        print(f"[{level}] {message}", file=sys.stderr)

    # -- base handlers ----------------------------------------------------
    def on_initialize(self, params):
        client_version = params.get("protocol_version", 0)
        return {"protocol_version": min(PROTOCOL_VERSION, client_version),
                "plugin": self.info, "capabilities": self.capabilities}

    def on_shutdown(self, params):
        self._shutdown = True
        return {}

    def on_disconnect(self):
        """Called when the serve loop ends; release hardware here."""
