"""Ethereum Keccak-256 with optional accelerators and a hermetic fallback."""

from __future__ import annotations


_MASK64 = (1 << 64) - 1


_KECCAK_RATE_BYTES = 136  # Keccak-256: rate=1088 bits, capacity=512 bits.


_KECCAK_ROUND_CONSTANTS = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808A,
    0x8000000080008000,
    0x000000000000808B,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008A,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000A,
    0x000000008000808B,
    0x800000000000008B,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800A,
    0x800000008000000A,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
]


_KECCAK_ROTATION_OFFSETS = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
]


def _rotl64(value: int, shift: int) -> int:
    value &= _MASK64
    if shift == 0:
        return value
    return ((value << shift) | (value >> (64 - shift))) & _MASK64


def _keccak_f1600(state: list[int]) -> None:
    """Apply the Keccak-f[1600] permutation in-place."""
    for rc in _KECCAK_ROUND_CONSTANTS:
        c = [
            state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20]
            for x in range(5)
        ]
        d = [c[(x - 1) % 5] ^ _rotl64(c[(x + 1) % 5], 1) for x in range(5)]
        for y in range(5):
            for x in range(5):
                state[x + 5 * y] ^= d[x]

        b = [0] * 25
        for y in range(5):
            for x in range(5):
                b[y + 5 * ((2 * x + 3 * y) % 5)] = _rotl64(
                    state[x + 5 * y], _KECCAK_ROTATION_OFFSETS[x][y]
                )

        for y in range(5):
            for x in range(5):
                state[x + 5 * y] = (
                    b[x + 5 * y]
                    ^ ((~b[((x + 1) % 5) + 5 * y]) & b[((x + 2) % 5) + 5 * y])
                ) & _MASK64

        state[0] ^= rc


def _pure_python_keccak256(data: bytes) -> bytes:
    """Return Ethereum Keccak-256, not FIPS SHA3-256."""
    state = [0] * 25
    padded = bytearray(data)
    pad_len = _KECCAK_RATE_BYTES - (len(padded) % _KECCAK_RATE_BYTES)
    if pad_len == 1:
        padded.append(0x81)
    else:
        padded.append(0x01)
        padded.extend(b"\x00" * (pad_len - 2))
        padded.append(0x80)

    for offset in range(0, len(padded), _KECCAK_RATE_BYTES):
        block = padded[offset : offset + _KECCAK_RATE_BYTES]
        for lane in range(_KECCAK_RATE_BYTES // 8):
            start = lane * 8
            state[lane] ^= int.from_bytes(block[start : start + 8], "little")
        _keccak_f1600(state)

    return b"".join(lane.to_bytes(8, "little") for lane in state)[:32]


try:
    from Crypto.Hash import keccak as _keccak_mod  # pyright: ignore[reportMissingImports]

    def keccak256(data: bytes) -> bytes:
        return _keccak_mod.new(data=data, digest_bits=256).digest()
except ImportError:
    try:
        import sha3  # pyright: ignore[reportMissingImports]

        def keccak256(data: bytes) -> bytes:
            return sha3.keccak_256(data).digest()
    except ImportError:

        def keccak256(data: bytes) -> bytes:
            return _pure_python_keccak256(data)
