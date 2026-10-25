#!/usr/bin/env python3
"""Operator-only Jani mTLS registry administration. No private CA keys."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

FINGERPRINT = re.compile(r"[0-9a-f]{64}\Z")
IDENTITY = re.compile(r"[A-Za-z0-9._-]{1,96}\Z")
MAX_ENTRIES = 4096
MAX_SIZE = 1_048_576


def registry_read(path):
    if path.is_symlink():
        raise ValueError("registry symlink refused")
    if not path.exists():
        return {}
    if not path.is_file() or path.stat().st_size > MAX_SIZE:
        raise ValueError("registry not regular file or exceeds 1 MiB")
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or len(data) > MAX_ENTRIES:
        raise ValueError("invalid registry object")
    for fp, entry in data.items():
        if not FINGERPRINT.fullmatch(fp) or not isinstance(entry, dict):
            raise ValueError("invalid fingerprint or entry")
        if set(entry) - {"tenant_id", "device_id", "revoked", "expires_unix"}:
            raise ValueError("unknown registry fields")
        if not all(isinstance(entry.get(k), str) and IDENTITY.fullmatch(entry[k]) for k in ("tenant_id", "device_id")):
            raise ValueError("invalid tenant or device")
        if "revoked" in entry and not isinstance(entry["revoked"], bool):
            raise ValueError("invalid revoked flag")
        if "expires_unix" in entry and (not isinstance(entry["expires_unix"], int) or isinstance(entry["expires_unix"], bool)):
            raise ValueError("invalid expires_unix")
    return data


def registry_write(path, data):
    if path.is_symlink():
        raise ValueError("registry symlink refused")
    parent = path.parent
    parent.mkdir(parents=True, exist_ok=True)
    if len(data) > MAX_ENTRIES:
        raise ValueError("too many certificates")
    encoded = (json.dumps(data, indent=2, sort_keys=True) + "\n").encode()
    if len(encoded) > MAX_SIZE:
        raise ValueError("registry exceeds 1 MiB")
    fd, tmp = tempfile.mkstemp(prefix=".sentinel-registry-", dir=parent)
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(tmp, path)
        if os.name != "nt":
            directory_fd = os.open(parent, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def openssl(*args):
    proc = subprocess.run(["openssl", *map(str, args)], capture_output=True, timeout=15, check=False)
    if proc.returncode:
        raise ValueError("certificate validation failed")
    return proc.stdout


def credential(ca, cert):
    openssl("verify", "-CAfile", ca, "-purpose", "sslclient", cert)
    # A generic CA-issued certificate without explicit clientAuth EKU must not
    # automatically become a telemetry device identity.
    eku = openssl("x509", "-in", cert, "-noout", "-ext", "extendedKeyUsage").decode()
    if "TLS Web Client Authentication" not in eku and "clientAuth" not in eku:
        raise ValueError("certificate lacks explicit client authentication EKU")
    der = openssl("x509", "-in", cert, "-outform", "DER")
    end = openssl("x509", "-in", cert, "-noout", "-enddate").decode().strip()
    if not end.startswith("notAfter="):
        raise ValueError("missing certificate expiry")
    parsed = datetime.datetime.strptime(end[9:], "%b %d %H:%M:%S %Y %Z").replace(tzinfo=datetime.timezone.utc)
    expiry = int(parsed.timestamp())
    if expiry <= int(datetime.datetime.now(datetime.timezone.utc).timestamp()):
        raise ValueError("expired certificate")
    return hashlib.sha256(der).hexdigest(), expiry


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    a = sub.add_parser("enroll")
    a.add_argument("--registry", required=True, type=Path)
    a.add_argument("--ca", required=True)
    a.add_argument("--cert", required=True)
    a.add_argument("--tenant", required=True)
    a.add_argument("--device", required=True)
    a.add_argument("--expires-unix", type=int)
    a = sub.add_parser("revoke")
    a.add_argument("--registry", required=True, type=Path)
    a.add_argument("--fingerprint", required=True)
    a = sub.add_parser("list")
    a.add_argument("--registry", required=True, type=Path)
    args = parser.parse_args(argv)
    data = registry_read(args.registry)
    if args.action == "enroll":
        if not IDENTITY.fullmatch(args.tenant) or not IDENTITY.fullmatch(args.device):
            parser.error("invalid tenant or device identity")
        fp, cert_expiry = credential(args.ca, args.cert)
        if fp in data:
            raise ValueError("certificate already registered; rotate with a new certificate")
        effective = min(cert_expiry, args.expires_unix) if args.expires_unix is not None else cert_expiry
        if effective <= int(datetime.datetime.now(datetime.timezone.utc).timestamp()):
            raise ValueError("expired requested enrollment")
        data[fp] = {"tenant_id": args.tenant, "device_id": args.device, "revoked": False,
                    "expires_unix": effective}
        registry_write(args.registry, data)
        print(json.dumps({"fingerprint": fp, "tenant": args.tenant, "device": args.device, "expires_unix": effective}))
    elif args.action == "revoke":
        if not FINGERPRINT.fullmatch(args.fingerprint):
            parser.error("invalid fingerprint")
        if args.fingerprint not in data:
            raise ValueError("certificate not registered")
        data[args.fingerprint]["revoked"] = True
        registry_write(args.registry, data)
        print(json.dumps({"fingerprint": args.fingerprint, "revoked": True}))
    else:
        print(json.dumps([{"fingerprint": k, **v} for k, v in sorted(data.items())], indent=2))


if __name__ == "__main__":
    main()
