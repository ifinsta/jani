"""Bounded, content-checked input for release evidence."""

from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat


MAX_DOCUMENT_BYTES = 4 * 1024 * 1024
MAX_ARTIFACT_BYTES = 32 * 1024**3
CHUNK_BYTES = 64 * 1024
WINDOWS_DEVICE_NAMES = {"CON", "PRN", "AUX", "NUL"} | {
    f"{device}{number}" for device in ("COM", "LPT") for number in range(10)
}


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def reject_constant(_value):
    raise ValueError("non-finite JSON number")


def decode_object(raw):
    value = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields,
                       parse_constant=reject_constant)
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


@contextmanager
def regular_input(path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise ValueError("expected a plain, regular evidence file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    with os.fdopen(os.open(path, flags), "rb") as stream:
        opened = os.fstat(stream.fileno())
        if not stat.S_ISREG(opened.st_mode) or not os.path.samestat(metadata, opened):
            raise ValueError("evidence file changed while opening")
        yield stream, opened.st_size


def read_object(path):
    with regular_input(path) as (stream, size):
        if size > MAX_DOCUMENT_BYTES:
            raise ValueError("evidence document exceeds its size limit")
        raw = stream.read(MAX_DOCUMENT_BYTES + 1)
    if len(raw) != size:
        raise ValueError("evidence document changed while reading")
    return decode_object(raw), raw


def sha256_string(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def artifact_parts(reference):
    if not isinstance(reference, str) or len(reference) > 255:
        raise ValueError("invalid evidence artifact path")
    parts = reference.split("/")
    if any(not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", part)
           or part.endswith(".") or part.split(".", 1)[0].upper() in WINDOWS_DEVICE_NAMES
           for part in parts):
        raise ValueError("invalid evidence artifact path")
    return parts


def artifact_path(root, parts):
    path = root
    for part in parts:
        path = path / part
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
            raise ValueError("redirected evidence artifact path")
    if not path.resolve(strict=True).is_relative_to(root):
        raise ValueError("evidence artifact escapes its root")
    return path


@contextmanager
def rooted_input(root, reference):
    parts = artifact_parts(reference)
    root = root.resolve(strict=True)
    if (os.open not in os.supports_dir_fd or not hasattr(os, "O_DIRECTORY")
            or not hasattr(os, "O_NOFOLLOW")):
        # On Windows the verifier expects an immutable operator-owned snapshot.
        with regular_input(artifact_path(root, parts)) as opened:
            yield opened
        return

    # Resolve children relative to an opened directory, so an intermediate
    # pathname cannot redirect a subsequent open outside that directory.
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    with ExitStack() as stack:
        directory = os.open(root, directory_flags)
        stack.callback(os.close, directory)
        for part in parts[:-1]:
            directory = os.open(part, directory_flags, dir_fd=directory)
            stack.callback(os.close, directory)
        metadata = os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError("expected a plain, regular evidence artifact")
        flags = os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_NONBLOCK", 0)
        with os.fdopen(os.open(parts[-1], flags, dir_fd=directory), "rb") as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode) or not os.path.samestat(metadata, opened):
                raise ValueError("evidence artifact changed while opening")
            yield stream, opened.st_size


@dataclass(frozen=True)
class Artifact:
    path: str
    sha256: str
    size: int

    @classmethod
    def from_record(cls, value, *, path_field="path"):
        if not isinstance(value, dict):
            raise ValueError("missing evidence artifact descriptor")
        path, digest, size = value.get(path_field), value.get("sha256"), value.get("bytes")
        if not isinstance(path, str) or not sha256_string(digest):
            raise ValueError("invalid evidence artifact descriptor")
        if type(size) is not int or not 0 < size <= MAX_ARTIFACT_BYTES:
            raise ValueError("invalid evidence artifact size")
        return cls(path, digest, size)


class HashingInput:
    def __init__(self, stream, size):
        self.stream = stream
        self.remaining = size
        self.digest = hashlib.sha256()

    def read(self, count):
        if count < 0:
            raise ValueError("evidence reads must specify a bounded size")
        data = self.stream.read(min(count, self.remaining + 1))
        if len(data) > self.remaining:
            raise ValueError("evidence artifact grew while being read")
        self.digest.update(data)
        self.remaining -= len(data)
        return data


@contextmanager
def checked_artifact(root, artifact):
    with rooted_input(root, artifact.path) as (stream, size):
        if size != artifact.size:
            raise ValueError("evidence artifact size mismatch")
        checked = HashingInput(stream, size)
        yield checked
        while checked.read(CHUNK_BYTES):
            pass
        if checked.remaining != 0 or checked.digest.hexdigest() != artifact.sha256:
            raise ValueError("evidence artifact SHA-256 mismatch")
