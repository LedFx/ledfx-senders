"""Run with python -I: stdlib-only installed Hue cancellation smoke on loopback."""

import faulthandler
import json
import os
import socket
import sys
import sysconfig
import threading
from collections.abc import Callable
from pathlib import Path
from typing import TypeVar, cast

T = TypeVar("T")
FREE_THREADED = bool(sysconfig.get_config_var("Py_GIL_DISABLED"))
GIL_ENABLED = cast(Callable[[], bool], getattr(sys, "_is_gil_enabled", lambda: True))


def assert_gil() -> None:
    if FREE_THREADED:
        assert not GIL_ENABLED(), "Hue operation enabled the GIL"


def checked(operation: Callable[[], T]) -> T:
    assert_gil()
    try:
        return operation()
    finally:
        assert_gil()


def main() -> None:
    faulthandler.dump_traceback_later(10, exit=True)
    if os.environ.get("LEDFX_REQUIRE_FREE_THREADING") == "1":
        assert FREE_THREADED, "Expected a free-threaded interpreter"
    assert_gil()
    from ledfx_senders import HueSender, _native

    assert_gil()  # Import itself must preserve free-threading.
    extension = Path(_native.__file__).resolve()
    checkout = Path(__file__).resolve().parents[1]
    assert not extension.is_relative_to(checkout), extension
    baseline_threads = set(threading.enumerate())
    failures: list[Exception] = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
        listener.bind(("127.0.0.1", 0))
        listener.settimeout(2)
        sender = checked(
            lambda: HueSender(
                destination="127.0.0.1",
                port=listener.getsockname()[1],
                psk_identity=b"wheel-smoke-dummy",
                client_key=bytes(range(16)),
                entertainment_id="12345678-1234-1234-1234-123456789abc",
                channel_ids=(7,),
            )
        )
        assert not checked(lambda: sender.connected)
        assert not checked(lambda: sender.closed)
        checked(sender.service)
        try:
            checked(lambda: sender.send(bytes(3)))
        except ConnectionError:
            pass
        else:
            raise AssertionError("unconnected session accepted a frame")

        def connect() -> None:
            try:
                checked(sender.connect)
            except (ConnectionError, TimeoutError) as exc:
                failures.append(exc)

        worker = threading.Thread(target=connect, name="hue-smoke-connect", daemon=True)
        worker.start()
        try:
            datagram, address = listener.recvfrom(65535)
            assert datagram and address[0] == "127.0.0.1"
            checked(sender.close)
        finally:
            checked(sender.close)
            worker.join(0.5)
        assert not worker.is_alive(), "close failed to interrupt native connect"
        assert len(failures) == 1, failures
        checked(sender.close)
        assert checked(lambda: sender.closed)
        assert not checked(lambda: sender.connected)
        for operation in (
            sender.connect,
            lambda: sender.send(bytes(3)),
            sender.service,
        ):
            try:
                checked(operation)
            except ConnectionError:
                pass
            else:
                raise AssertionError("closed session accepted an operation")
    assert set(threading.enumerate()) == baseline_threads, "smoke worker leaked"
    assert_gil()
    faulthandler.cancel_dump_traceback_later()
    print(
        json.dumps(
            {
                "extension": str(extension),
                "free_threaded": FREE_THREADED,
                "gil_enabled": GIL_ENABLED(),
                "connected": checked(lambda: sender.connected),
                "closed": checked(lambda: sender.closed),
                "connect_error": type(failures[0]).__name__,
                "worker_joined": True,
            }
        )
    )


if __name__ == "__main__":
    main()
