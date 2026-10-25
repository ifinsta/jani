#!/usr/bin/env python3
"""Install an existing locally committed Jani macOS/Linux release, with SHA-256 verification."""
import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

TYPES = {'agent': 'jani', 'client': 'jani-probe', 'gateway': 'jani-gateway'}
SEMVER = re.compile(r'^v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][a-zA-Z0-9.+-]+)?$')


def native_target():
    system = platform.system().lower()
    machine = platform.machine().lower()
    cpu = {'x86_64': 'x86_64', 'amd64': 'x86_64', 'aarch64': 'aarch64', 'arm64': 'aarch64'}.get(machine)
    if not cpu:
        raise ValueError('unsupported CPU; provide --target explicitly')
    if system == 'darwin':
        return 'macos', f'{cpu}-apple-darwin'
    if system == 'linux':
        if not platform.libc_ver()[0]:
            raise ValueError('cannot identify libc; pass --target for a verified target')
        return 'linux', f'{cpu}-unknown-linux-gnu'
    raise ValueError('this installer only supports macOS and Linux; use install.ps1 on Windows')


def verify(root: Path, kind: str, target: str, version: str | None):
    system = 'macos' if 'darwin' in target else 'linux' if 'linux' in target else None
    if not system or kind not in TYPES:
        raise ValueError('unsupported component or host target')
    directory = root / 'releases' / kind / system
    if version is None:
        pointer = json.loads((directory / 'latest.json').read_text())
        if pointer != {'schemaVersion':'jani.release-channel/v2','type':kind,'platform':system,'latest':pointer.get('latest')}:
            raise ValueError('release channel mismatch')
        version = pointer['latest']
    if not version.startswith('v'):
        version = 'v' + version
    if not SEMVER.fullmatch(version):
        raise ValueError('invalid version')
    folder = directory / version
    m = json.loads((folder / 'manifest.json').read_text())
    if m['schemaVersion'] != 'jani.release/v2' or m['type'] != kind or m['platform'] != system or m['tag'] != version or m['package'] != TYPES[kind]:
        raise ValueError('manifest identity mismatch')
    artifact = next((a for a in m['artifacts'] if a['target'] == target), None)
    if not artifact:
        raise ValueError(f'no verified {kind}/{system}/{version} artifact for {target}')
    name = artifact['binary']
    if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9._+-]+', name) or '..' in name:
        raise ValueError('unsafe binary filename')
    source = folder / name
    if source.is_symlink() or not source.is_file():
        raise ValueError('missing or symlinked release binary')
    h = hashlib.sha256(source.read_bytes()).hexdigest()
    if h != artifact['sha256'] or source.stat().st_size != artifact['sizeBytes']:
        raise ValueError('artifact checksum/size mismatch')
    sidecar = folder / (name + '.sha256')
    if sidecar.is_symlink() or sidecar.read_text() != f'{h}  {name}\n':
        raise ValueError('checksum sidecar mismatch')
    return source, TYPES[kind], h


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--type', choices=tuple(TYPES), default='agent')
    parser.add_argument('--version')
    parser.add_argument('--target')
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--install-dir', type=Path, default=Path.home() / '.local/bin')
    args = parser.parse_args()
    try:
        _, target = native_target() if not args.target else (None, args.target)
        source, name, h = verify(args.root, args.type, target, args.version)
        dest_dir = args.install_dir.expanduser().resolve()
        dest_dir.mkdir(parents=True, exist_ok=True)
        dest = dest_dir / name
        if dest.is_symlink():
            raise ValueError('refusing symlinked existing destination')
        with tempfile.NamedTemporaryFile(dir=dest_dir, prefix=f'.{name}.', delete=False) as f:
            tmp = Path(f.name)
            with source.open('rb') as src:
                shutil.copyfileobj(src, f)
            f.flush()
            os.fsync(f.fileno())
        try:
            tmp.chmod(0o755)
            subprocess.run([str(tmp), '--version'], check=True, capture_output=True, timeout=10)
            os.replace(tmp, dest)
        finally:
            tmp.unlink(missing_ok=True)
        print(f'installed {dest} from {source} (SHA-256 {h}); release is not code-signed')
        return 0
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as e:
        print(f'install-unix: {e}', file=sys.stderr)
        return 1

if __name__ == '__main__':
    sys.exit(main())
