#!/usr/bin/env python3
"""Generate the bundled UI CJK fallback font subsets.

Inputs must be the unmodified upstream Noto Sans CJK regional subset OTFs.
The generated outputs are Modified Versions under the OFL, so this script
renames the primary OpenType and CFF names after subsetting.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
FONT_DIR = ROOT / "crates/app/ui/fonts"
I18N_DIR = ROOT / "crates/app/i18n"
EXPECTED_SOURCE_SHA256 = {
    "sc": "faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9",
    "kr": "69975a0ac8472717870aefeab0a4d52739308d90856b9955313b2ad5e0148d68",
}
IGNORED_TEXT_CONTROLS = {"\r", "\n", "\t"}


ASSETS = [
    {
        "key": "sc",
        "locale": "zh-cn",
        "sample": "\u7b80\u4f53\u4e2d\u6587 \u6211\u7684\u521b\u610f\u5de5\u574a\u7269\u54c1 \u6a21\u7ec4\u5927\u5c0f\u5206\u6790 \u6b63\u5728\u4e0b\u8f7d",
        "output": "GMPCJKSCUI-Regular.otf",
        "family": "GMP CJKSC UI",
        "unique": "2.004;GMP;GMPCJKSCUI-Regular",
        "postscript": "GMPCJKSCUI-Regular",
        "ascii_replacements": {
            b"NotoSansSC-Regular": b"GMPCJKSCUI-Regular",
            b"Noto Sans SC Regular": b"GMP CJKSC-UI Regular",
            b"Noto Sans SC": b"GMP CJKSC UI",
        },
    },
    {
        "key": "kr",
        "locale": "kr",
        "sample": "\ud55c\uad6d\uc5b4 \ub0b4 \ucc3d\uc791\ub9c8\ub2f9 \uc560\ub4dc\uc628 \ud06c\uae30 \ubd84\uc11d \ub2e4\uc6b4\ub85c\ub4dc \uc911",
        "output": "GMPCJKKRUI-Regular.otf",
        "family": "GMP CJKKR UI",
        "unique": "2.004;GMP;GMPCJKKRUI-Regular",
        "postscript": "GMPCJKKRUI-Regular",
        "ascii_replacements": {
            b"NotoSansKR-Regular": b"GMPCJKKRUI-Regular",
            b"Noto Sans KR Regular": b"GMP CJKKR-UI Regular",
            b"Noto Sans KR": b"GMP CJKKR UI",
        },
    },
]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sc-source", type=Path, required=True)
    parser.add_argument("--kr-source", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, default=FONT_DIR)
    parser.add_argument("--hb-subset", type=Path, default=default_hb_subset())
    args = parser.parse_args()

    sources = {"sc": args.sc_source, "kr": args.kr_source}
    args.output_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="gmpublished-cjk-subset-") as temp_dir:
        temp_root = Path(temp_dir)
        for asset in ASSETS:
            source = sources[asset["key"]]
            verify_source_hash(asset["key"], source)
            text_file = temp_root / f"{asset['locale']}.txt"
            text_file.write_text(required_text(asset["locale"], asset["sample"]), encoding="utf-8")
            subset_file = temp_root / f"{asset['output']}.subset"
            run_hb_subset(args.hb_subset, source, text_file, subset_file)
            output = args.output_dir / asset["output"]
            patch_modified_font_names(subset_file, output, asset)
            print_measure(output)


def default_hb_subset() -> Path:
    configured = os.environ.get("HB_SUBSET")
    if configured:
        return Path(configured)
    homebrew = Path("/opt/homebrew/bin/hb-subset")
    if homebrew.exists():
        return homebrew
    found = shutil.which("hb-subset")
    if found:
        return Path(found)
    return homebrew


def verify_source_hash(key: str, path: Path) -> None:
    actual = sha256(path)
    expected = EXPECTED_SOURCE_SHA256[key]
    if actual != expected:
        raise SystemExit(f"{path} SHA-256 mismatch: expected {expected}, got {actual}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def required_text(locale: str, sample: str) -> str:
    catalog = json.loads((I18N_DIR / f"{locale}.json").read_text(encoding="utf-8"))
    chars: set[str] = set()
    collect_string_chars(catalog, chars)
    chars.update(character for character in sample if character not in IGNORED_TEXT_CONTROLS)
    return "".join(sorted(chars))


def collect_string_chars(value: Any, chars: set[str]) -> None:
    if isinstance(value, str):
        chars.update(character for character in value if character not in IGNORED_TEXT_CONTROLS)
    elif isinstance(value, list):
        for item in value:
            collect_string_chars(item, chars)
    elif isinstance(value, dict):
        for item in value.values():
            collect_string_chars(item, chars)


def run_hb_subset(hb_subset: Path, source: Path, text_file: Path, output: Path) -> None:
    subprocess.run(
        [
            str(hb_subset),
            str(source),
            f"--text-file={text_file}",
            "--no-hinting",
            f"--output-file={output}",
        ],
        check=True,
    )


def patch_modified_font_names(source: Path, output: Path, asset: dict[str, Any]) -> None:
    data = bytearray(source.read_bytes())
    records = table_records(data)
    patch_name_table(data, records, asset)
    patch_ascii_names(data, asset["ascii_replacements"])
    refresh_checksums(data)
    output.write_bytes(data)


def patch_name_table(data: bytearray, records: dict[str, tuple[int, int, int]], asset: dict[str, Any]) -> None:
    _, name_offset, _ = records["name"]
    table_format, record_count, string_offset = struct.unpack(">HHH", data[name_offset : name_offset + 6])
    if table_format != 0:
        raise SystemExit(f"unsupported name table format {table_format}")
    replacements = {
        1: asset["family"],
        3: asset["unique"],
        4: asset["family"],
        6: asset["postscript"],
    }
    for index in range(record_count):
        record_offset = name_offset + 6 + index * 12
        platform, _, _, name_id, old_length, old_relative_offset = struct.unpack(
            ">HHHHHH", data[record_offset : record_offset + 12]
        )
        if name_id not in replacements:
            continue
        if platform not in (0, 3):
            raise SystemExit(f"unexpected name table platform {platform}")
        replacement = replacements[name_id].encode("utf-16-be")
        if len(replacement) > old_length:
            raise SystemExit(f"name ID {name_id} replacement is too long")
        text_offset = name_offset + string_offset + old_relative_offset
        data[text_offset : text_offset + old_length] = b"\0" * old_length
        data[text_offset : text_offset + len(replacement)] = replacement
        data[record_offset + 8 : record_offset + 10] = struct.pack(">H", len(replacement))


def patch_ascii_names(data: bytearray, replacements: dict[bytes, bytes]) -> None:
    for old, new in replacements.items():
        if len(old) != len(new):
            raise SystemExit(f"replacement must preserve CFF string length: {old!r} -> {new!r}")
        if old not in data:
            raise SystemExit(f"expected CFF/name string not found: {old!r}")
        data[:] = data.replace(old, new)


def table_records(data: bytearray) -> dict[str, tuple[int, int, int]]:
    table_count = struct.unpack(">H", data[4:6])[0]
    records = {}
    for index in range(table_count):
        record_offset = 12 + index * 16
        tag = bytes(data[record_offset : record_offset + 4]).decode("ascii")
        _, offset, length = struct.unpack(">III", data[record_offset + 4 : record_offset + 16])
        records[tag] = (record_offset, offset, length)
    return records


def refresh_checksums(data: bytearray) -> None:
    records = table_records(data)
    _, head_offset, _ = records["head"]
    data[head_offset + 8 : head_offset + 12] = b"\0\0\0\0"
    for _, (record_offset, table_offset, table_length) in records.items():
        table_checksum = checksum(data[table_offset : table_offset + table_length])
        data[record_offset + 4 : record_offset + 8] = struct.pack(">I", table_checksum)
    adjustment = (0xB1B0AFBA - checksum(data)) & 0xFFFFFFFF
    data[head_offset + 8 : head_offset + 12] = struct.pack(">I", adjustment)


def checksum(data) -> int:
    total = 0
    padded = bytes(data) + b"\0" * ((4 - len(data) % 4) % 4)
    for offset in range(0, len(padded), 4):
        total = (total + struct.unpack(">I", padded[offset : offset + 4])[0]) & 0xFFFFFFFF
    return total


def print_measure(path: Path) -> None:
    print(f"{path} bytes={path.stat().st_size} sha256={sha256(path)}")


if __name__ == "__main__":
    main()
