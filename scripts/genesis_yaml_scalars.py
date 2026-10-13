"""Scalars and lexical rules for the strict network YAML subset."""

from __future__ import annotations
from pathlib import Path
from typing import Any


class YamlError(ValueError):
    def __init__(self, path: Path, line_no: int, message: str):
        super().__init__(f"{path}:{line_no}: {message}")


def _parse_scalar(raw: str, *, path: Path, line_no: int) -> Any:
    raw = raw.strip()
    if not raw:
        raise YamlError(path, line_no, "empty value; write an explicit scalar")
    if raw[0] in "\"'":
        quote = raw[0]
        if len(raw) < 2 or raw[-1] != quote or raw.count(quote) != 2:
            raise YamlError(path, line_no, "unterminated or nested quotes")
        return raw[1:-1]
    literals = {"[]": [], "{}": {}, "true": True, "false": False}
    if raw in literals:
        return literals[raw]
    for marker in ("&", "*", "|", ">", "{", "[", "%", "@", "`"):
        if raw.startswith(marker):
            raise YamlError(
                path,
                line_no,
                f"unsupported YAML construct {marker!r}; use plain key: value "
                f"(only the empty literals [] and {{}} are accepted inline)",
            )
    if raw in ("null", "~"):
        raise YamlError(path, line_no, "null values are not supported; omit the key")
    try:
        return int(raw, 10)
    except ValueError:
        return raw


def _split_key(line: str, *, path: Path, line_no: int) -> tuple[str, str]:
    # A key ends at the first ':' followed by a space or end of line, so hex
    # values and `host:port` entries stay unambiguous.
    for pos, char in enumerate(line):
        if char == ":" and (pos + 1 == len(line) or line[pos + 1] == " "):
            key = line[:pos].strip()
            if not key:
                raise YamlError(path, line_no, "empty mapping key")
            if key[0] in "\"'":
                parsed = _parse_scalar(key, path=path, line_no=line_no)
                if not isinstance(parsed, str):
                    raise YamlError(path, line_no, "mapping key must be a string")
                key = parsed
            return key, line[pos + 1 :].strip()
    raise YamlError(path, line_no, "expected `key: value` or `key:`")


def _strip_comment(line: str) -> str:
    in_quote: str | None = None
    for pos, char in enumerate(line):
        if in_quote:
            if char == in_quote:
                in_quote = None
        elif char in "\"'":
            in_quote = char
        elif char == "#" and (pos == 0 or line[pos - 1] in " \t"):
            return line[:pos]
    return line


def _looks_like_mapping(body: str) -> bool:
    for pos, char in enumerate(body):
        if char == ":" and (pos + 1 == len(body) or body[pos + 1] == " "):
            return True
    return False
