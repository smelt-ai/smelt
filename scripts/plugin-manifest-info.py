#!/usr/bin/env python3
"""Validate the manifest fields consumed by shell packaging scripts."""

import json
import re
import sys
from pathlib import Path


IDENTIFIER = re.compile(r"^[A-Za-z0-9._:-]{1,255}$")
ENTRYPOINT_SEGMENT = re.compile(r"^[A-Za-z0-9._-]+$")


def fail(path, message):
    print(f"invalid plugin manifest {path}: {message}", file=sys.stderr)
    raise SystemExit(2)


def required_string(manifest, path, field):
    value = manifest.get(field)
    if not isinstance(value, str):
        fail(path, f"{field} must be a string")
    return value


def validate_entrypoint(path, entrypoint):
    prefix = "bin/"
    if not entrypoint.startswith(prefix):
        fail(path, f"entrypoint must start with {prefix}")
    relative = entrypoint[len(prefix) :]
    if (
        len(entrypoint) > 1024
        or not relative
        or "\\" in entrypoint
        or any(
            not segment
            or segment in {".", ".."}
            or ENTRYPOINT_SEGMENT.fullmatch(segment) is None
            for segment in relative.split("/")
        )
    ):
        fail(path, "entrypoint is invalid")


def main():
    if len(sys.argv) != 2:
        print(f"usage: {Path(sys.argv[0]).name} <plugin.json>", file=sys.stderr)
        raise SystemExit(2)
    path = Path(sys.argv[1])
    try:
        with path.open(encoding="utf-8") as stream:
            manifest = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        fail(path, f"cannot parse JSON: {error}")
    if not isinstance(manifest, dict):
        fail(path, "root must be an object")

    plugin_id = required_string(manifest, path, "id")
    if IDENTIFIER.fullmatch(plugin_id) is None:
        fail(path, "id is invalid")
    if "execution" in manifest:
        fail(path, "execution is obsolete")
    bundled = manifest.get("bundled", True)
    if not isinstance(bundled, bool):
        fail(path, "bundled must be a boolean")
    entrypoint = required_string(manifest, path, "entrypoint")
    validate_entrypoint(path, entrypoint)

    print("\t".join((plugin_id, entrypoint, str(bundled).lower())))


if __name__ == "__main__":
    main()
