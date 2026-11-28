#!/usr/bin/env python3
"""Create, verify, and restore a cold snapshot of one Jani state directory."""

from __future__ import annotations

import argparse
from contextlib import closing, nullcontext
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import struct
import sys
import time
from typing import BinaryIO

SCHEMA = "jani.state-backup/v1"
RESTORE_HOLD_FILE = "restore-hold.json"
CHUNK_BYTES = 1024 * 1024
MAX_MANIFEST_BYTES = 64 * CHUNK_BYTES
SQLITE_COMPANIONS = ("-wal", "-journal", "-shm")
RUNTIME_DATABASES = {
    Path("attention/state.sqlite3"): "attention_state",
    Path("mirror/improvements.sqlite3"): "mirror_findings",
    Path("native/native.sqlite3"): "objects",
    Path("tasks/state.sqlite3"): "task_sessions",
}


class BackupError(Exception):
    pass


def checked_kind(info: os.stat_result, path: Path) -> str:
    if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
        raise BackupError(f"link or reparse point refused: {path}")
    if stat.S_ISDIR(info.st_mode):
        return "directory"
    if stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
        return "file"
    raise BackupError(f"hardlinked or special entry refused: {path}")


def fingerprint(info: os.stat_result) -> tuple[int, ...]:
    fields = (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size, info.st_mtime_ns)
    # Windows lstat and fstat can report different ctime values for the same file.
    return fields if os.name == "nt" else (*fields, info.st_ctime_ns)


def directory(path: Path) -> Path:
    absolute = path.absolute()
    for part in (*reversed(absolute.parents), absolute):
        if checked_kind(part.lstat(), part) != "directory":
            raise BackupError(f"expected a directory: {part}")
    return absolute.resolve(strict=True)


def inventory(root: Path) -> dict[Path, os.stat_result]:
    root_info = root.lstat()
    if checked_kind(root_info, root) != "directory":
        raise BackupError(f"expected a directory: {root}")
    entries = {Path(): root_info}
    pending = [Path()]
    while pending:
        parent = pending.pop()
        with os.scandir(root / parent) as children:
            for child in children:
                relative = parent / child.name
                info = (root / relative).lstat()
                kind = checked_kind(info, root / relative)
                if info.st_dev != root_info.st_dev:
                    raise BackupError(f"different filesystem refused: {relative}")
                entries[relative] = info
                if kind == "directory":
                    pending.append(relative)
    return entries


def require_unchanged(root: Path, initial: dict[Path, os.stat_result]) -> None:
    current = inventory(root)
    if current.keys() != initial.keys() or any(
        fingerprint(info) != fingerprint(current[path]) for path, info in initial.items()
    ):
        raise BackupError("input tree changed during the operation")


def open_input(path: Path, expected: os.stat_result) -> BinaryIO:
    handle = os.open(path, os.O_RDONLY | getattr(os, "O_BINARY", 0) |
                     getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    source = os.fdopen(handle, "rb")
    try:
        require_same_file(source, path, expected)
    except BaseException:
        source.close()
        raise
    return source


def require_same_file(source: BinaryIO, path: Path, expected: os.stat_result) -> None:
    opened = os.fstat(source.fileno())
    if checked_kind(opened, path) != "file" or fingerprint(opened) != fingerprint(expected):
        raise BackupError(f"input file changed: {path}")


def create_file(path: Path) -> BinaryIO:
    return os.fdopen(
        os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0), 0o600), "wb"
    )


def sync_file(file: BinaryIO) -> None:
    file.flush()
    os.fsync(file.fileno())


def stream_entry(path: Path, info: os.stat_result, destination: Path | None) -> dict:
    digest = hashlib.sha256()
    size = 0
    executable = os.name == "posix" and bool(info.st_mode & stat.S_IXUSR)
    target_context = create_file(destination) if destination is not None else nullcontext()
    with target_context as target, open_input(path, info) as source:
        while chunk := source.read(CHUNK_BYTES):
            size += len(chunk)
            if size > info.st_size:
                raise BackupError(f"input file grew: {path}")
            digest.update(chunk)
            if target is not None:
                target.write(chunk)
        if size != info.st_size:
            raise BackupError(f"input file was truncated: {path}")
        require_same_file(source, path, info)
        if target is not None:
            if executable:
                os.fchmod(target.fileno(), 0o700)
            sync_file(target)
    return {"size": size, "sha256": digest.hexdigest(), "executable": executable}


def tree_entries(root: Path, entries: dict[Path, os.stat_result], target: Path | None = None,
                 omit: frozenset[Path] = frozenset()) -> dict:
    records = {}
    for relative, info in entries.items():
        if relative == Path() or relative in omit:
            continue
        output = target / relative if target is not None else None
        if stat.S_ISDIR(info.st_mode):
            if output is not None:
                output.mkdir(mode=0o700)
            records[relative.as_posix()] = None
        else:
            records[relative.as_posix()] = stream_entry(root / relative, info, output)
    return records


def sqlite_paths(entries: dict[Path, os.stat_result]) -> tuple[list[Path], frozenset[Path]]:
    databases = []
    shared_memory = set()
    for path in RUNTIME_DATABASES:
        if path in entries:
            if not stat.S_ISREG(entries[path].st_mode) or entries[path].st_size == 0:
                raise BackupError(f"runtime database must be a nonempty regular file: {path}")
            databases.append(path)
        for suffix in SQLITE_COMPANIONS:
            companion = path.with_name(path.name + suffix)
            if companion not in entries:
                continue
            if path not in entries or not stat.S_ISREG(entries[companion].st_mode):
                raise BackupError(f"SQLite companion lacks a regular database or is not a file: {companion}")
            if suffix == "-journal":
                raise BackupError(f"unexpected rollback journal for a WAL-mode runtime database: {companion}")
            if suffix == "-shm":
                wal = path.with_name(path.name + "-wal")
                if wal not in entries or entries[wal].st_size == 0:
                    raise BackupError(f"SQLite shared memory has no matching nonempty WAL: {companion}")
                shared_memory.add(companion)
    return databases, frozenset(shared_memory)


def normalize_database(path: Path, required_table: str) -> None:
    wal = path.with_name(path.name + "-wal")
    if wal.exists():
        validate_wal(wal)
    # SQLite rebuilds its SHM index from the WAL on the private copy.
    with closing(sqlite3.connect(path.as_uri() + "?mode=rw", uri=True)) as db:
        db.execute("PRAGMA trusted_schema=OFF")
        db.execute("PRAGMA synchronous=FULL")
        if db.execute("PRAGMA journal_mode=DELETE").fetchone() != ("delete",):
            raise BackupError(f"SQLite did not finish journal recovery: {path.name}")
        if db.execute("PRAGMA integrity_check(1)").fetchone() != ("ok",):
            raise BackupError(f"SQLite integrity check failed: {path.name}")
        if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise BackupError(f"SQLite foreign key check failed: {path.name}")
        if db.execute("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?", (required_table,)).fetchone() is None:
            raise BackupError(f"runtime database lacks expected table {required_table}: {path.name}")
    if any(path.with_name(path.name + suffix).exists() for suffix in SQLITE_COMPANIONS):
        raise BackupError(f"SQLite left a companion outside the recovered database: {path.name}")
    with path.open("r+b") as file:
        sync_file(file)


def wal_checksum(order: str, data: bytes, seed: tuple[int, int]) -> tuple[int, int]:
    first, second = seed
    for a, b in struct.iter_unpack(order + "II", data):
        first = (first + a + second) & 0xFFFFFFFF
        second = (second + b + first) & 0xFFFFFFFF
    return first, second


def validate_wal(path: Path) -> None:
    """Check physical WAL bytes before SQLite can discard a damaged suffix.

    Format reference: https://www.sqlite.org/fileformat2.html#walformat
    """
    with path.open("rb") as wal:
        header = wal.read(32)
        if not header:
            return  # A checkpoint can leave an empty WAL.
        if len(header) != 32:
            raise BackupError(f"incomplete SQLite WAL header: {path}")
        magic, version, page_size = struct.unpack(">III", header[:12])
        if (magic not in (0x377F0682, 0x377F0683) or version != 3007000 or
                not 512 <= page_size <= 65536 or page_size & (page_size - 1)):
            raise BackupError(f"unsupported or corrupt SQLite WAL header: {path}")
        order = "<" if magic == 0x377F0682 else ">"
        checksum = wal_checksum(order, header[:24], (0, 0))
        if checksum != struct.unpack(">II", header[24:]):
            raise BackupError(f"SQLite WAL header checksum mismatch: {path}")
        frame = 0
        while frame_header := wal.read(24):
            frame += 1
            page = wal.read(page_size)
            if len(frame_header) != 24 or len(page) != page_size:
                raise BackupError(f"truncated SQLite WAL frame {frame}: {path}")
            if frame_header[8:16] != header[16:24] or frame_header[:4] == b"\0" * 4:
                raise BackupError(f"ambiguous SQLite WAL frame {frame}: {path}")
            checksum = wal_checksum(order, page, wal_checksum(order, frame_header[:8], checksum))
            if checksum != struct.unpack(">II", frame_header[16:24]):
                raise BackupError(f"SQLite WAL frame {frame} checksum mismatch: {path}")


def sync_directory(path: Path) -> None:
    if os.name == "posix":
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def sync_tree(root: Path, entries: dict[Path, os.stat_result]) -> None:
    for path, info in reversed(entries.items()):
        if stat.S_ISDIR(info.st_mode):
            sync_directory(root / path)


def reserve(requested: Path, *sources: Path) -> Path:
    version = sys.version_info[:3]
    if os.name == "nt" and (version < (3, 11, 10) or (3, 12) <= version < (3, 12, 4)):
        raise BackupError("Windows needs Python 3.11.10+, 3.12.4+, or 3.13+ for private output directories")
    destination = directory(requested.parent) / requested.name
    if not requested.name or any(destination == source or source in destination.parents for source in sources):
        raise BackupError("output must be outside the input tree")
    destination.mkdir(mode=0o700)
    return destination


def write_json(path: Path, value: dict) -> str:
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(",", ":")).encode("ascii") + b"\n"
    if len(encoded) > MAX_MANIFEST_BYTES:
        raise BackupError("snapshot manifest exceeds the size limit")
    with create_file(path) as output:
        output.write(encoded)
        sync_file(output)
    return hashlib.sha256(encoded).hexdigest()


def backup(state: Path, output: Path) -> dict:
    source = directory(state)
    initial = inventory(source)
    if not any(stat.S_ISREG(info.st_mode) for info in initial.values()):
        raise BackupError("state directory contains no files")
    databases, shared_memory = sqlite_paths(initial)
    destination = reserve(output, source)
    payload = destination / "state"
    payload.mkdir(mode=0o700)
    tree_entries(source, initial, payload, shared_memory)
    require_unchanged(source, initial)
    for path in databases:
        normalize_database(payload / path, RUNTIME_DATABASES[path])
    saved = inventory(payload)
    manifest = {"schema": SCHEMA, "sourceStateDir": str(source), "entries": tree_entries(payload, saved)}
    sync_tree(payload, saved)
    pin = write_json(destination / ".manifest-pending", manifest)
    (destination / ".manifest-pending").rename(destination / "manifest.json")
    sync_directory(destination)
    sync_directory(destination.parent)
    return {"backup": str(destination), "manifestSha256": pin, "entries": len(saved) - 1,
            "sqliteDatabases": len(databases)}


def read_manifest(archive: Path, pin: str) -> tuple[dict, Path]:
    if re.fullmatch(r"[0-9a-f]{64}", pin) is None:
        raise BackupError("manifest SHA-256 must be 64 lowercase hexadecimal characters")
    path = archive / "manifest.json"
    info = path.lstat()
    if checked_kind(info, path) != "file" or info.st_size > MAX_MANIFEST_BYTES:
        raise BackupError("manifest is not a bounded regular file")
    with open_input(path, info) as file:
        encoded = file.read(MAX_MANIFEST_BYTES + 1)
        require_same_file(file, path, info)
    if not hmac.compare_digest(hashlib.sha256(encoded).hexdigest(), pin):
        raise BackupError("manifest SHA-256 does not match the operator's retained digest")
    manifest = json.loads(encoded)
    if (not isinstance(manifest, dict) or set(manifest) != {"schema", "entries", "sourceStateDir"} or
            manifest["schema"] != SCHEMA or not isinstance(manifest["entries"], dict) or
            not isinstance(manifest["sourceStateDir"], str)):
        raise BackupError("unsupported snapshot manifest")
    original = Path(manifest["sourceStateDir"])
    if not original.is_absolute():
        raise BackupError("snapshot state path is incompatible with this host")
    return manifest["entries"], original


def inspect_backup(archive: Path, pin: str, output: Path | None = None) -> dict:
    archive = directory(archive)
    records, original = read_manifest(archive, pin)
    if {path.name for path in archive.iterdir()} != {"state", "manifest.json"}:
        raise BackupError("snapshot container has unexpected entries")
    source = archive / "state"
    initial = inventory(source)
    if {path.as_posix() for path in initial if path != Path()} != records.keys():
        raise BackupError("snapshot paths differ from the manifest")
    target = reserve(output, archive, original) if output is not None else None
    pending = target / ".restore-pending" if target is not None else None
    if pending is not None:
        pending.mkdir(mode=0o700)
    observed = tree_entries(source, initial, pending)
    require_unchanged(source, initial)
    if observed != records or (pending is not None and tree_entries(pending, inventory(pending)) != records):
        raise BackupError("snapshot content differs from the manifest")
    result = {"verified": True, "manifestSha256": pin, "entries": len(records)}
    if target is not None:
        previous_switch = pending / "kill-switch.json"
        if previous_switch.exists():
            if not previous_switch.is_file():
                raise BackupError("previous kill switch is not a file")
            saved_switch = previous_switch.rename(target / "prior-kill-switch.json")
            result["priorKillSwitch"] = str(saved_switch)
        hold = {"reason": f"Restored snapshot {pin} requires operator reconciliation",
                "actor": "operator-disaster-recovery", "activatedAt": int(time.time())}
        write_json(target / ".hold-pending", hold)
        (target / ".hold-pending").replace(pending / "kill-switch.json")
        write_json(target / ".hold-pending", {"schema": SCHEMA, "manifestSha256": pin})
        (target / ".hold-pending").replace(pending / RESTORE_HOLD_FILE)
        sync_tree(pending, inventory(pending))
        restored = pending.rename(target / "state")
        sync_directory(target)
        sync_directory(target.parent)
        result.update(stateDir=str(restored), reconciliationRequired=True)
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("backup", help="copy a stopped runtime's state")
    create.add_argument("--state-dir", type=Path, required=True)
    create.add_argument("--out", type=Path, required=True)
    create.add_argument("--offline", action="store_true", required=True,
                        help="confirm that all runtime and other state writers are stopped")
    for command in ("verify", "restore"):
        reader = commands.add_parser(command)
        reader.add_argument("--backup", type=Path, required=True)
        reader.add_argument("--manifest-sha256", required=True)
        if command == "restore":
            reader.add_argument("--out", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "backup":
            result = backup(args.state_dir, args.out)
        else:
            result = inspect_backup(args.backup, args.manifest_sha256, getattr(args, "out", None))
        print(json.dumps(result, indent=2))
    except (BackupError, OSError, sqlite3.Error, ValueError, RecursionError) as error:
        print(f"state backup: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
