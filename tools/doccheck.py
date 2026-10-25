#!/usr/bin/env python3
"""Validate Jani's source-linked normative Markdown documentation manifest."""
import argparse
import json
import re
import sys
from pathlib import Path

CORE = tuple("docs/" + x + ".md" for x in (
    "PROJECT_CONTRACT", "THREAT_MODEL", "AUTHORIZATION_MATRIX",
    "TOOL_EXECUTION_CONTRACT", "AGENT_LIFECYCLE", "STORAGE_CONSISTENCY",
    "RECOVERY_AND_REPLAY", "DELEGATION_PROTOCOL", "ERRORS_AND_RETRIES",
    "CONFIGURATION_REFERENCE", "RELEASE_ACCEPTANCE", "COMPONENT_CONTRACTS"
))
SENTINEL = ("docs/SENTINEL_CONTRACT.md", "docs/SENTINEL_RELEASE_GATES.md")
STATUSES = {"implemented", "implemented-partial", "proposed", "deprecated", "superseded"}
REQUIRED = {"id", "title", "status", "owner", "version", "last-reviewed", "source", "tests"}
SAFE = re.compile(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*$")


def rooted(root: Path, rel: object) -> Path:
    if not isinstance(rel, str) or not SAFE.fullmatch(rel) or any(part in {".", ".."} for part in rel.split("/")):
        raise ValueError("unsafe or non-relative path: " + repr(rel))
    path = root / rel
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError("path escapes repository: " + rel)
    current = root
    for component in rel.split("/"):
        current = current / component
        if current.is_symlink():
            raise ValueError("symlink refused: " + rel)
    return path


def frontmatter(text: str) -> dict:
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("missing frontmatter")
    try:
        stop = lines.index("---", 1)
    except ValueError as exc:
        raise ValueError("unterminated frontmatter") from exc
    fields = {}
    active = None
    for line in lines[1:stop]:
        if line.startswith("  - ") and active in ("source", "tests"):
            if not isinstance(fields[active], list):
                raise ValueError("list item after scalar")
            fields[active].append(line[4:].strip())
        else:
            match = re.fullmatch(r"([a-z-]+):(?: (.*))?", line)
            if not match:
                raise ValueError("invalid frontmatter line: " + line)
            key, value = match.groups()
            if key in fields:
                raise ValueError("duplicate field: " + key)
            if key in ("source", "tests"):
                fields[key] = [] if not value or value == "[]" else [value]
            else:
                fields[key] = (value or "").strip('"')
            active = key
    return fields


def check(root: Path) -> list[str]:
    root = root.resolve()
    errors = []
    try:
        manifest = json.loads((root / "docs/documentation_manifest.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        return ["manifest missing or invalid: " + str(exc)]
    if manifest.get("schemaVersion") != "jani.docs/v1":
        errors.append("unsupported documentation manifest version")
    paths, critical = manifest.get("files"), manifest.get("requiredCore")
    if not isinstance(paths, list) or not all(isinstance(p, str) for p in paths):
        return errors + ["manifest files must be a list of strings"]
    if not isinstance(critical, list) or set(critical) != set(CORE):
        errors.append("requiredCore differs from mandatory twelve contracts")
    if manifest.get("requiredSentinel") != list(SENTINEL):
        errors.append("requiredSentinel differs from mandatory monitoring security gates")
    if set(SENTINEL) - set(paths):
        errors.append("mandatory Sentinel contract missing from manifest")
    if len(paths) != len(set(paths)):
        errors.append("duplicate manifest path")
    if set(CORE) - set(paths):
        errors.append("mandatory contract missing from manifest")
    ids = set()
    for rel in paths:
        try:
            path = rooted(root, rel)
            if not rel.endswith(".md") or not path.is_file():
                raise ValueError("not an existing Markdown document")
            meta = frontmatter(path.read_text(encoding="utf-8"))
            missing = REQUIRED - set(meta)
            if missing:
                raise ValueError("missing fields " + ", ".join(sorted(missing)))
            if not re.fullmatch(r"JANI-[A-Z]+-[0-9]{3}", meta["id"]):
                raise ValueError("invalid document ID")
            if meta["id"] in ids:
                raise ValueError("duplicate document ID: " + meta["id"])
            ids.add(meta["id"])
            if meta["status"] not in STATUSES:
                raise ValueError("unsupported document status")
            if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", meta["last-reviewed"]):
                raise ValueError("invalid review date")
            if not meta["title"] or not meta["owner"] or not meta["version"]:
                raise ValueError("empty title, owner or version")
            if not isinstance(meta["source"], list) or not meta["source"]:
                raise ValueError("source list must not be empty")
            if not isinstance(meta["tests"], list):
                raise ValueError("tests must be a list")
            if meta["status"] == "implemented" and not meta["tests"]:
                raise ValueError("implemented requires test references")
            for reference in meta["source"] + meta["tests"]:
                target = rooted(root, reference)
                if not target.is_file():
                    raise ValueError("missing source/test reference: " + reference)
        except (ValueError, OSError, TypeError, KeyError) as exc:
            errors.append(rel + ": " + str(exc))
    # A new frontmatter-bearing Markdown file must be registered, not silently excluded.
    for file in root.rglob("*.md"):
        if any(part in {".git", ".jani", "target", "claims", "node_modules", "generated"} for part in file.relative_to(root).parts):
            continue
        if file.is_symlink() or not file.is_file():
            continue
        rel = file.relative_to(root).as_posix()
        try:
            with file.open("r", encoding="utf-8") as stream:
                first = stream.readline().strip()
        except (OSError, UnicodeError) as exc:
            errors.append(rel + ": unreadable: " + str(exc))
            continue
        if first == "---" and rel not in paths:
            errors.append(rel + ": frontmatter document absent from manifest")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    errors = check(args.root)
    if errors:
        for error in errors:
            print("doccheck: " + error, file=sys.stderr)
        return 1
    count = len(json.loads((args.root / "docs/documentation_manifest.json").read_text())["files"])
    print("doccheck: validated " + str(count) + " normative documents and twelve core contracts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
