# SPDX-License-Identifier: Apache-2.0
"""Unit tests: OCP framing codec."""

import io
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from openmso.framing import MessageStream, ProtocolError


def roundtrip(messages):
    buf = io.BytesIO()
    out = MessageStream(io.BytesIO(), buf)
    for msg, payload in messages:
        out.write_message(msg, payload)
    inp = MessageStream(io.BytesIO(buf.getvalue()), io.BytesIO())
    got = []
    while (item := inp.read_message()) is not None:
        got.append(item)
    return got


def test_plain_messages():
    got = roundtrip([({"a": 1}, None), ({"b": [1, 2]}, None)])
    assert got == [({"a": 1}, None), ({"b": [1, 2]}, None)]


def test_binary_payload():
    payload = bytes(range(256)) * 100
    got = roundtrip([({"method": "capture.data"}, payload), ({"end": True}, None)])
    assert got[0][0]["binlen"] == len(payload)
    assert got[0][1] == payload
    assert got[1] == ({"end": True}, None)


def test_payload_containing_newlines():
    payload = b"\n\n{\"fake\":1}\n" * 50
    got = roundtrip([({"m": 1}, payload), ({"m": 2}, None)])
    assert got[0][1] == payload
    assert got[1][0]["m"] == 2


def test_eof_inside_payload_raises():
    buf = io.BytesIO()
    out = MessageStream(io.BytesIO(), buf)
    out.write_message({"m": 1}, b"x" * 100)
    truncated = buf.getvalue()[:-40]
    inp = MessageStream(io.BytesIO(truncated), io.BytesIO())
    with pytest.raises(ProtocolError):
        inp.read_message()


def test_bad_json_raises():
    inp = MessageStream(io.BytesIO(b"{nope}\n"), io.BytesIO())
    with pytest.raises(ProtocolError):
        inp.read_message()
