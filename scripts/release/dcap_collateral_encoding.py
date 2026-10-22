from __future__ import annotations

import re


def extract_type5_pck_chain(quote: bytes) -> bytes:
    """Return the exact type-5 certification data from a complete SGX quote v3."""
    fixed_quote_size = 436
    fixed_authentication_size = 64 + 64 + 384 + 64
    if len(quote) < fixed_quote_size + fixed_authentication_size + 8:
        raise ValueError("quote is shorter than the SGX v3 authentication structure")
    if int.from_bytes(quote[0:2], "little") != 3:
        raise ValueError("quote version is not SGX quote v3")
    if int.from_bytes(quote[4:8], "little") != 0:
        raise ValueError("quote tee_type is not SGX")

    authentication_size = int.from_bytes(quote[432:436], "little")
    if fixed_quote_size + authentication_size != len(quote):
        raise ValueError("quote has trailing or truncated authentication bytes")

    cursor = fixed_quote_size + fixed_authentication_size
    qe_authentication_size = int.from_bytes(quote[cursor : cursor + 2], "little")
    cursor += 2 + qe_authentication_size
    if cursor + 6 > len(quote):
        raise ValueError("quote certification-data header is truncated")
    certification_data_type = int.from_bytes(quote[cursor : cursor + 2], "little")
    certification_data_size = int.from_bytes(quote[cursor + 2 : cursor + 6], "little")
    cursor += 6
    end = cursor + certification_data_size
    if certification_data_type != 5:
        raise ValueError("quote certification data is not type 5")
    if end != len(quote):
        raise ValueError("quote has trailing or truncated certification data")

    pck_chain = quote[cursor:end]
    if (
        not pck_chain.startswith(b"-----BEGIN CERTIFICATE-----\n")
        or not pck_chain.endswith(b"\0")
        or b"\0" in pck_chain[:-1]
    ):
        raise ValueError("type-5 PCK certificate chain is not canonical PEM plus NUL")
    return pck_chain


def canonical_text_component(value: bytes, label: str) -> bytes:
    """Remove the QPL ABI terminator while rejecting ambiguous text bytes."""
    if not value.endswith(b"\0") or value.endswith(b"\0\0") or b"\0" in value[:-1]:
        raise ValueError(f"{label} is not terminated by exactly one NUL")
    canonical = value[:-1]
    if not canonical:
        raise ValueError(f"{label} is empty")
    try:
        canonical.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError(f"{label} is not UTF-8") from error
    return canonical


def canonical_der_component(value: bytes, label: str) -> bytes:
    """Return one complete DER SEQUENCE, dropping only an ABI terminator."""
    if len(value) < 2 or value[0] != 0x30:
        raise ValueError(f"{label} is not a DER SEQUENCE")

    encoded_length = value[1]
    if encoded_length < 0x80:
        header_size = 2
        content_size = encoded_length
    else:
        length_size = encoded_length & 0x7F
        if length_size == 0 or length_size > 4 or len(value) < 2 + length_size:
            raise ValueError(f"{label} has an invalid DER length")
        length_bytes = value[2 : 2 + length_size]
        if length_bytes[0] == 0:
            raise ValueError(f"{label} has a non-canonical DER length")
        content_size = int.from_bytes(length_bytes, "big")
        if content_size < 0x80:
            raise ValueError(f"{label} has a non-canonical DER length")
        header_size = 2 + length_size

    object_size = header_size + content_size
    if len(value) == object_size:
        return value
    if len(value) == object_size + 1 and value[object_size] == 0:
        return value[:object_size]
    raise ValueError(f"{label} has trailing or truncated DER bytes")


def canonical_crl_component(value: bytes, minor_version: int, label: str) -> bytes:
    """Normalize legacy hex and size-delimited raw CRLs to canonical DER."""
    if minor_version not in (0, 1):
        raise ValueError(f"{label} uses unsupported collateral minor version")

    if value.startswith(b"\x30"):
        return canonical_der_component(value, label)
    if minor_version == 1:
        raise ValueError(f"{label} v3.1 payload is not raw DER")

    encoded = value[:-1] if value.endswith(b"\0") else value
    if not encoded or b"\0" in encoded:
        raise ValueError(f"{label} contains an invalid NUL")
    if len(encoded) % 2 != 0:
        raise ValueError(f"{label} contains odd-length hexadecimal DER")
    try:
        encoded_text = encoded.decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError(f"{label} is not canonical hexadecimal DER") from error
    if not re.fullmatch(r"[0-9A-Fa-f]+", encoded_text):
        raise ValueError(f"{label} is not canonical hexadecimal DER")
    return canonical_der_component(bytes.fromhex(encoded_text), label)
