"""E1.31 setup-time packet templates; frame sending belongs to the native engine."""

import struct

DEFAULT_PORT = 5568
DATA_PACKET_SIZE = 638
PAYLOAD_OFFSET = 126
DISCOVERY_ADDRESS = "239.255.250.214"


def _integer(value: int, minimum: int, maximum: int, name: str) -> None:
    if type(value) is not int:
        raise TypeError(f"{name} must be an integer")
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be between {minimum} and {maximum}")


def _name(source_name: str) -> bytes:
    if not isinstance(source_name, str):
        raise TypeError("source_name must be a string")
    encoded = source_name.encode("utf-8")
    if len(encoded) > 63 or b"\0" in encoded:
        raise ValueError("source_name must contain at most 63 UTF-8 bytes and no NUL")
    return encoded


def _root(size: int, cid: bytes, vector: int) -> bytearray:
    if not isinstance(cid, bytes):
        raise TypeError("cid must be bytes")
    if len(cid) != 16:
        raise ValueError("cid must contain exactly 16 bytes")
    packet = bytearray(size)
    struct.pack_into(
        "!HH12sHI16s",
        packet,
        0,
        16,
        0,
        b"ASC-E1.17\0\0\0",
        0x7000 | (size - 16),
        vector,
        cid,
    )
    return packet


def data_template(
    universe: int, *, cid: bytes, source_name: str, priority: int, sync_universe: int
) -> bytearray:
    """Create a full 512-slot level packet with zero sequence/options/payload."""
    _integer(universe, 1, 63999, "universe")
    _integer(priority, 0, 200, "priority")
    _integer(sync_universe, 0, 63999, "sync_universe")
    name = _name(source_name)
    packet = _root(DATA_PACKET_SIZE, cid, 4)
    struct.pack_into(
        "!HI64sBHBBH",
        packet,
        38,
        0x7258,
        2,
        name,
        priority,
        sync_universe,
        0,
        0,
        universe,
    )
    struct.pack_into("!HBBHHHB", packet, 115, 0x720B, 2, 0xA1, 0, 1, 513, 0)
    return packet


def sync_template(*, cid: bytes, sync_universe: int) -> bytearray:
    _integer(sync_universe, 1, 63999, "sync_universe")
    packet = _root(49, cid, 8)
    struct.pack_into("!HIBHH", packet, 38, 0x700B, 1, 0, sync_universe, 0)
    return packet


def discovery_packets(
    universes: tuple[int, ...], *, cid: bytes, source_name: str
) -> tuple[bytes, ...]:
    """Announce unique sorted universes, at most 512 entries per page."""
    name = _name(source_name)
    for universe in universes:
        _integer(universe, 1, 63999, "universe")
    ordered = sorted(set(universes))
    pages = max(1, (len(ordered) + 511) // 512)
    packets = []
    for page in range(pages):
        values = ordered[page * 512 : (page + 1) * 512]
        size = 120 + 2 * len(values)
        packet = _root(size, cid, 8)
        struct.pack_into("!HI64s", packet, 38, 0x7000 | (size - 38), 2, name)
        struct.pack_into(
            "!HIBB", packet, 112, 0x7000 | (size - 112), 1, page, pages - 1
        )
        for index, universe in enumerate(values):
            struct.pack_into("!H", packet, 120 + 2 * index, universe)
        packets.append(bytes(packet))
    return tuple(packets)


def multicast_address(universe: int) -> str:
    _integer(universe, 1, 63999, "universe")
    return f"239.255.{universe >> 8}.{universe & 255}"
