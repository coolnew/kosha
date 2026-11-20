#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Windows release policy: static CRT and bounded PE import inspection (no tools required)."""

import os
from pathlib import Path
import re
import struct

TARGET = "x86_64-pc-windows-msvc"
REDIST = re.compile(r"(?:vcruntime|msvcp|msvcr|concrt|vcomp|vccorlib)\d[^/\\]*\.dll", re.I)


def build_environment(environment=None):
    result = dict(os.environ if environment is None else environment)
    flags = (result.get("CARGO_ENCODED_RUSTFLAGS", "").split("\x1f")
             if "CARGO_ENCODED_RUSTFLAGS" in result else result.get("RUSTFLAGS", "").split())
    result["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([f for f in flags if f] + ["-C", "target-feature=+crt-static"])
    result.pop("RUSTFLAGS", None)
    result["CARGO_BUILD_TARGET"] = TARGET
    return result


def imports(data: bytes) -> list[str]:
    """Read x64 PE normal AND delay-load import names; reject malformed inputs."""
    def unpack(fmt, offset):
        size = struct.calcsize(fmt)
        if offset < 0 or offset + size > len(data):
            raise ValueError("Truncated PE image")
        return struct.unpack_from(fmt, data, offset)
    if data[:2] != b"MZ":
        raise ValueError("Not a PE executable")
    pe = unpack("<I", 0x3c)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("Invalid PE signature")
    machine, count = unpack("<HH", pe + 4)
    optional_size = unpack("<H", pe + 20)[0]
    optional = pe + 24
    if machine != 0x8664 or unpack("<H", optional)[0] != 0x20b or not 1 <= count <= 96 or optional_size < 112:
        raise ValueError("Expected an x64 PE32+ image")
    image_base = unpack("<Q", optional + 24)[0]
    headers = unpack("<I", optional + 60)[0]
    directories = unpack("<I", optional + 108)[0]
    if directories > (optional_size - 112) // 8:
        raise ValueError("Invalid PE data-directory count")
    sections = []
    for i in range(count):
        virtual_size, address, raw_size, raw = unpack("<IIII", optional + optional_size + i * 40 + 8)
        if raw + raw_size > len(data):
            raise ValueError("PE section exceeds file")
        sections.append((address, raw_size, raw))
    def offset(rva, size):
        if 0 < rva < headers and rva + size <= min(headers, len(data)):
            return rva
        for address, raw_size, raw in sections:
            if address <= rva and rva + size <= address + raw_size:
                return raw + rva - address
        raise ValueError("PE address does not map to file bytes")
    def name(rva):
        start = offset(rva, 1)
        end = data.find(b"\0", start, start + 257)
        if end < 0:
            raise ValueError("Unterminated PE DLL name")
        offset(rva, end - start + 1)
        try:
            value = data[start:end].decode("ascii").lower()
        except UnicodeDecodeError:
            raise ValueError("Invalid PE DLL name") from None
        if not re.fullmatch(r"[a-z0-9_.+-]+\.dll", value):
            raise ValueError("Unsafe PE DLL name")
        return value
    names = set()
    for index, stride in [(1, 20), (13, 32)]:
        if index >= directories:
            continue
        address, size = unpack("<II", optional + 112 + index * 8)
        if not address and not size:
            continue
        if not address or size < stride or size // stride > 4096:
            raise ValueError("Invalid PE import directory")
        for i in range(size // stride):
            values = unpack("<" + "I" * (stride // 4), offset(address + i * stride, stride))
            if not any(values):
                break
            rva = values[3] if index == 1 else values[1]
            if index == 13 and not values[0] & 1:
                rva -= image_base
            names.add(name(rva))
        else:
            raise ValueError("Unterminated PE import directory")
    return sorted(names)


def verify_bytes(data: bytes) -> list[str]:
    names = imports(data)
    forbidden = [n for n in names if REDIST.fullmatch(n) or n == "ucrtbased.dll"]
    if forbidden:
        raise ValueError("External Visual C++ runtime dependency: " + ", ".join(forbidden))
    return names


def verify_folder(folder: Path) -> dict:
    return {name: verify_bytes((folder / name).read_bytes()) for name in ["kosha.exe", "kosha-gui.exe"]}
