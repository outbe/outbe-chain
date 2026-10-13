"""The strict network YAML subset, with one parser cursor and explicit nesting."""

from __future__ import annotations
from pathlib import Path
from typing import Any
from genesis_yaml_scalars import (
    YamlError,
    _parse_scalar,
    _split_key,
    _strip_comment,
    _looks_like_mapping,
)


class _Parser:
    def __init__(self, path: Path):
        self.path = path
        self.lines = _numbered_lines(path)
        self.pos = 0

    def parse(self) -> dict[str, Any]:
        result = self.mapping(0)
        if self.pos != len(self.lines):
            raise YamlError(
                self.path, self.lines[self.pos][0], "could not parse from this line on"
            )
        return result

    def mapping(self, indent: int) -> dict[str, Any]:
        mapping = {}
        while self.pos < len(self.lines):
            line_no, line_indent, content = self.lines[self.pos]
            if line_indent < indent:
                break
            if line_indent > indent:
                raise YamlError(self.path, line_no, "unexpected indentation")
            if content.startswith("- "):
                raise YamlError(
                    self.path, line_no, "list item where a mapping key was expected"
                )
            key, rest = _split_key(content, path=self.path, line_no=line_no)
            if key in mapping:
                raise YamlError(self.path, line_no, f"duplicate key {key!r}")
            self.pos += 1
            mapping[key] = self.mapping_value(key, rest, indent, line_no)
        return mapping

    def mapping_value(self, key: str, rest: str, indent: int, line_no: int) -> Any:
        if rest:
            return _parse_scalar(rest, path=self.path, line_no=line_no)
        if self.pos >= len(self.lines) or self.lines[self.pos][1] <= indent:
            raise YamlError(self.path, line_no, f"key {key!r} has no value")
        child_indent = self.lines[self.pos][1]
        if self.lines[self.pos][2].startswith("- "):
            return self.sequence(child_indent)
        return self.mapping(child_indent)

    def sequence(self, indent: int) -> list[Any]:
        items = []
        while self.pos < len(self.lines):
            line_no, line_indent, content = self.lines[self.pos]
            if line_indent < indent:
                break
            if line_indent > indent or not content.startswith("- "):
                raise YamlError(self.path, line_no, "inconsistent list indentation")
            body = content[2:].strip()
            if not body:
                raise YamlError(self.path, line_no, "empty list item")
            self.pos += 1
            if _looks_like_mapping(body):
                items.append(self.flat_mapping(body, indent, line_no))
            else:
                items.append(_parse_scalar(body, path=self.path, line_no=line_no))
        return items

    def flat_mapping(self, body: str, indent: int, line_no: int) -> dict[str, Any]:
        key, rest = _split_key(body, path=self.path, line_no=line_no)
        if not rest:
            raise YamlError(self.path, line_no, f"list item key {key!r} has no value")
        item = {key: _parse_scalar(rest, path=self.path, line_no=line_no)}
        while self.pos < len(self.lines):
            next_no, next_indent, content = self.lines[self.pos]
            if next_indent != indent + 2 or content.startswith("- "):
                break
            key, rest = _split_key(content, path=self.path, line_no=next_no)
            if key in item:
                raise YamlError(self.path, next_no, f"duplicate key {key!r}")
            if not rest:
                raise YamlError(
                    self.path,
                    next_no,
                    "nested structures inside list items are not supported",
                )
            item[key] = _parse_scalar(rest, path=self.path, line_no=next_no)
            self.pos += 1
        return item


def _numbered_lines(path: Path) -> list[tuple[int, int, str]]:
    numbered = []
    for line_no, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if "\t" in raw:
            raise YamlError(path, line_no, "tabs are not allowed; use spaces")
        stripped = _strip_comment(raw).rstrip()
        if stripped.strip():
            indent = len(stripped) - len(stripped.lstrip(" "))
            numbered.append((line_no, indent, stripped.strip()))
    return numbered


def load_yaml(path: Path) -> dict[str, Any]:
    """Parse the supported YAML subset into plain dicts/lists/scalars."""
    return _Parser(path).parse()
