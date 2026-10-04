"""Wire-format checks use literal E1.31 offsets, independent of the encoder."""

from pathlib import Path
from typing import cast

import pytest
from ledfx_senders import e131_packet as codec

CID = bytes(range(16))
SOURCE = "LedFx-test"
FIXTURES = Path(__file__).parent / "fixtures" / "e131"


def data() -> bytearray:
    return codec.data_template(
        17, cid=CID, source_name=SOURCE, priority=177, sync_universe=63999
    )


def test_data_reference_and_literal_fields():
    packet = data()
    assert isinstance(packet, bytearray)
    assert len(packet) == 638
    assert packet[:16] == bytes.fromhex("001000004153432d45312e3137000000")
    assert packet[16:18] == bytes.fromhex("726e")
    assert packet[18:22] == bytes.fromhex("00000004")
    assert packet[22:38] == CID
    assert packet[38:44] == bytes.fromhex("725800000002")
    assert packet[44:108] == SOURCE.encode() + bytes(54)
    assert packet[108:115] == bytes.fromhex("b1f9ff00000011")
    assert packet[115:126] == bytes.fromhex("720b02a100000001020100")
    assert packet[126:] == bytes(512)
    packet[126:] = bytes(range(256)) * 2
    assert packet == (FIXTURES / "data.bin").read_bytes()
    packet[112] = 0x40
    assert packet == (FIXTURES / "termination.bin").read_bytes()


def test_sync_reference():
    packet = codec.sync_template(cid=CID, sync_universe=63999)
    assert isinstance(packet, bytearray)
    assert len(packet) == 49
    assert packet[16:22] == bytes.fromhex("702100000008")
    assert packet[38:] == bytes.fromhex("700b0000000100f9ff0000")
    assert packet == (FIXTURES / "sync.bin").read_bytes()


def test_discovery_reference_and_pagination():
    packets = codec.discovery_packets((63999, 17, 17), cid=CID, source_name=SOURCE)
    assert packets == ((FIXTURES / "discovery.bin").read_bytes(),)
    packets = codec.discovery_packets(tuple(range(1, 514)), cid=CID, source_name=SOURCE)
    assert [len(p) for p in packets] == [1144, 122]
    for i, p in enumerate(packets):
        assert p[18:22] == bytes.fromhex("00000008")
        assert p[40:44] == bytes.fromhex("00000002")
        assert p[108:112] == bytes(4)
        assert int.from_bytes(p[16:18], "big") == 0x7000 | (len(p) - 16)
        assert int.from_bytes(p[38:40], "big") == 0x7000 | (len(p) - 38)
        assert int.from_bytes(p[112:114], "big") == 0x7000 | (len(p) - 112)
        assert p[114:118] == bytes.fromhex("00000001")
        assert p[118:120] == bytes([i, 1])
    assert packets[0][120:124] == bytes.fromhex("00010002")
    assert packets[1][120:] == bytes.fromhex("0201")
    empty = codec.discovery_packets((), cid=CID, source_name=SOURCE)
    assert len(empty) == 1 and len(empty[0]) == 120
    assert empty[0][118:120] == bytes(2)


@pytest.mark.parametrize("name", ["x" * 64, "é" * 32, "a\0b"])
def test_invalid_source(name: str) -> None:
    with pytest.raises(ValueError):
        codec.data_template(1, cid=CID, source_name=name, priority=100, sync_universe=0)
    with pytest.raises(ValueError):
        codec.discovery_packets((), cid=CID, source_name=name)


def test_utf8_boundary():
    p = codec.data_template(
        1, cid=CID, source_name="é" * 31 + "a", priority=0, sync_universe=0
    )
    assert p[44:108] == ("é" * 31 + "a").encode() + b"\0"


@pytest.mark.parametrize("universe", [0, 64000, -1, 65535, True, 1.5])
def test_invalid_universes(universe: object) -> None:
    universe = cast(int, universe)
    with pytest.raises((ValueError, TypeError)):
        codec.data_template(
            universe, cid=CID, source_name=SOURCE, priority=100, sync_universe=0
        )
    with pytest.raises((ValueError, TypeError)):
        codec.multicast_address(universe)
    with pytest.raises((ValueError, TypeError)):
        codec.discovery_packets((universe,), cid=CID, source_name=SOURCE)
    with pytest.raises((ValueError, TypeError)):
        codec.sync_template(cid=CID, sync_universe=universe)


@pytest.mark.parametrize("cid", [bytes(15), bytes(17), tuple(range(16)), "0" * 16])
def test_malformed_cid(cid: object) -> None:
    cid = cast(bytes, cid)
    with pytest.raises((ValueError, TypeError)):
        codec.data_template(
            1, cid=cid, source_name=SOURCE, priority=100, sync_universe=0
        )
    with pytest.raises((ValueError, TypeError)):
        codec.sync_template(cid=cid, sync_universe=1)
    with pytest.raises((ValueError, TypeError)):
        codec.discovery_packets((), cid=cid, source_name=SOURCE)


@pytest.mark.parametrize("priority", [-1, 201, True, 1.5])
def test_invalid_priority(priority: object) -> None:
    priority = cast(int, priority)
    with pytest.raises((ValueError, TypeError)):
        codec.data_template(
            1, cid=CID, source_name=SOURCE, priority=priority, sync_universe=0
        )


@pytest.mark.parametrize("sync", [-1, 64000, True, 1.5])
def test_invalid_data_sync(sync: object) -> None:
    sync = cast(int, sync)
    with pytest.raises((ValueError, TypeError)):
        codec.data_template(
            1, cid=CID, source_name=SOURCE, priority=100, sync_universe=sync
        )


def test_constants_and_bounds():
    assert (codec.DEFAULT_PORT, codec.DATA_PACKET_SIZE, codec.PAYLOAD_OFFSET) == (
        5568,
        638,
        126,
    )
    assert codec.DISCOVERY_ADDRESS == "239.255.250.214"
    assert codec.multicast_address(1) == "239.255.0.1"
    assert codec.multicast_address(63999) == "239.255.249.255"


def test_independent_decoder_and_malformed_lengths() -> None:
    from library_e131_oracle import decode_packet

    for name in ("data", "termination", "sync", "discovery"):
        packet = (FIXTURES / f"{name}.bin").read_bytes()
        assert decode_packet(packet)["cid"] == CID
        for malformed in (
            packet[:-1],
            packet + b"\0",
            packet[:16] + b"\0\0" + packet[18:],
        ):
            with pytest.raises(ValueError):
                decode_packet(malformed)
    assert (
        decode_packet((FIXTURES / "data.bin").read_bytes())["payload"]
        == bytes(range(256)) * 2
    )
