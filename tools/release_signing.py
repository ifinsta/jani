#!/usr/bin/env python3
"""Sign and verify Jani release manifests with OpenSSL Ed25519 keys."""
import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

DOMAIN = b"jani-release-manifest/v2\0"
SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")
HEX64 = re.compile(r"^[0-9a-fA-F]{64}$")
HEX_SIGNATURE = re.compile(r"^[0-9a-fA-F]{128}$")


def signing_bytes(manifest):
    unsigned = dict(manifest)
    unsigned.pop("signature", None)
    canonical = json.dumps(
        unsigned,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return DOMAIN + canonical


def parse_trust_key(spec):
    key_id, separator, value = spec.partition(":")
    if not separator:
        key_id, value = "default", key_id
    key_id, value = key_id.strip(), value.strip()
    if not key_id or not HEX64.fullmatch(value):
        raise ValueError("update keys must be ID:HEX or HEX with a 32-byte public key")
    return key_id, value.lower()


def run_openssl(arguments, **kwargs):
    result = subprocess.run(
        ["openssl", *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        **kwargs,
    )
    if result.returncode != 0:
        message = result.stderr.decode("utf-8", "replace").strip()
        raise ValueError(f"OpenSSL failed: {message or 'unknown error'}")
    return result.stdout


def public_key_hex(private_key):
    der = run_openssl(
        ["pkey", "-in", str(private_key), "-pubout", "-outform", "DER"]
    )
    if len(der) != len(SPKI_PREFIX) + 32 or not der.startswith(SPKI_PREFIX):
        raise ValueError("private key is not an Ed25519 key")
    return der[len(SPKI_PREFIX):].hex()


def sign_manifest(manifest, private_key, key_id):
    if not key_id or key_id.strip() != key_id:
        raise ValueError("release key id must be nonempty and have no surrounding whitespace")
    message = signing_bytes(manifest)
    with tempfile.TemporaryDirectory(prefix="jani-release-sign-") as directory:
        path = Path(directory) / "message.bin"
        path.write_bytes(message)
        signature = run_openssl(
            [
                "pkeyutl",
                "-sign",
                "-inkey",
                str(private_key),
                "-rawin",
                "-in",
                str(path),
            ]
        )
    if len(signature) != 64:
        raise ValueError("OpenSSL returned an invalid Ed25519 signature")
    signed = dict(manifest)
    signed["signature"] = {"keyId": key_id, "value": signature.hex()}
    return signed


def verify_manifest(manifest, trusted_keys):
    signature = manifest.get("signature")
    if not isinstance(signature, dict) or set(signature) != {"keyId", "value"}:
        raise ValueError("release manifest is unsigned or has an invalid signature object")
    key_id = signature["keyId"]
    value = signature["value"]
    if not isinstance(key_id, str) or not key_id or not isinstance(value, str) or not HEX_SIGNATURE.fullmatch(value):
        raise ValueError("release manifest has an invalid signature")
    matching = [hex_value for spec in trusted_keys for name, hex_value in [parse_trust_key(spec)] if name == key_id]
    if not matching:
        raise ValueError(f"no trusted release key with id '{key_id}'")
    message = signing_bytes(manifest)
    with tempfile.TemporaryDirectory(prefix="jani-release-verify-") as directory:
        root = Path(directory)
        (root / "message.bin").write_bytes(message)
        (root / "signature.bin").write_bytes(bytes.fromhex(value))
        for index, hex_value in enumerate(matching):
            public_key = root / f"public-{index}.der"
            public_key.write_bytes(SPKI_PREFIX + bytes.fromhex(hex_value))
            try:
                run_openssl(
                    [
                        "pkeyutl",
                        "-verify",
                        "-pubin",
                        "-inkey",
                        str(public_key),
                        "-rawin",
                        "-in",
                        str(root / "message.bin"),
                        "-sigfile",
                        str(root / "signature.bin"),
                    ]
                )
                return key_id
            except ValueError:
                continue
    raise ValueError("release manifest signature verification failed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    public = commands.add_parser("public-key")
    public.add_argument("--key", type=Path, required=True)
    sign = commands.add_parser("sign")
    sign.add_argument("--manifest", type=Path, required=True)
    sign.add_argument("--key", type=Path, required=True)
    sign.add_argument("--key-id", required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--update-key", action="append", required=True)
    args = parser.parse_args()
    try:
        if args.command == "public-key":
            print(public_key_hex(args.key))
        elif args.command == "sign":
            manifest = json.loads(args.manifest.read_text("utf-8"))
            signed = sign_manifest(manifest, args.key, args.key_id)
            content = json.dumps(signed, indent=2, sort_keys=True) + "\n"
            tmp = args.manifest.with_name(f".{args.manifest.name}.tmp.{id(content)}")
            tmp.write_text(content, "utf-8")
            tmp.replace(args.manifest)
            print(f"signed {args.manifest} with key id {args.key_id}")
        else:
            manifest = json.loads(args.manifest.read_text("utf-8"))
            print(f"verified {args.manifest} with key id {verify_manifest(manifest, args.update_key)}")
        return 0
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"release-signing: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
