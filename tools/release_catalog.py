#!/usr/bin/env python3
"""Validate Jani's immutable typed/platform release manifests (stdlib only)."""
import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

TYPES = {'agent': 'jani', 'client': 'jani-probe', 'gateway': 'jani-gateway'}
PLATFORMS = ('windows', 'macos', 'linux')
VERSION = re.compile(r'^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:[-+][0-9A-Za-z.+-]+)?$')
HEX = re.compile(r'^[0-9a-f]{64}$')
COMMIT = re.compile(r'^[0-9a-f]{40}$')


def safe_name(name):
    return isinstance(name, str) and name not in {'.', '..'} and bool(re.fullmatch(r'[A-Za-z0-9._+-]+', name)) and '..' not in name


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def check(root: Path):
    root = root.resolve()
    errors, releases = [], []
    base = root / 'releases'
    for typ, binary in TYPES.items():
        for platform in PLATFORMS:
            platform_dir = base / typ / platform
            if not platform_dir.exists():
                continue  # No fabricated release for unbuilt products.
            if platform_dir.is_symlink() or not platform_dir.is_dir():
                errors.append(f'{typ}/{platform}: invalid release directory')
                continue
            found = set()
            for d in platform_dir.iterdir():
                if d.is_symlink():
                    errors.append(f'{d}: symlink refused')
                    continue
                if not d.is_dir():
                    if d.name != 'latest.json':
                        errors.append(f'{d}: unexpected file')
                    continue
                if not VERSION.fullmatch(d.name):
                    errors.append(f'{d}: invalid version directory')
                    continue
                manifest_path = d / 'manifest.json'
                try:
                    m = json.loads(manifest_path.read_text('utf-8'))
                    if m['schemaVersion'] != 'jani.release/v2' or m['type'] != typ or m['platform'] != platform:
                        raise ValueError('type/platform/schema mismatch')
                    if m['version'] != d.name[1:] or m['tag'] != d.name:
                        raise ValueError('version mismatch')
                    if m['package'] != binary or not COMMIT.fullmatch(m['commit']):
                        raise ValueError('package or source commit invalid')
                    if not isinstance(m['dirty'], bool) or not isinstance(m['artifacts'], list) or not m['artifacts']:
                        raise ValueError('invalid build metadata or empty artifacts')
                    targets = set()
                    for a in m['artifacts']:
                        target, name = a['target'], a['binary']
                        if not isinstance(target, str) or platform not in (('windows' if 'windows' in target else 'macos' if 'darwin' in target else 'linux' if 'linux' in target else 'unknown'),):
                            raise ValueError('target does not match platform')
                        if not safe_name(name) or not name.startswith(f'{binary}-{m["version"]}-{target}'):
                            raise ValueError('unsafe or mismatched artifact name')
                        if target in targets or not HEX.fullmatch(a['sha256']) or a['commit'] != m['commit']:
                            raise ValueError('duplicate target, digest or commit mismatch')
                        targets.add(target)
                        artifact, checksum = d / name, d / (name + '.sha256')
                        if artifact.is_symlink() or not artifact.is_file() or checksum.is_symlink() or not checksum.is_file():
                            raise ValueError('missing artifact/checksum or symlink')
                        if artifact.stat().st_size != a['sizeBytes'] or digest(artifact) != a['sha256']:
                            raise ValueError('artifact bytes/hash mismatch')
                        if checksum.read_text('utf-8') != f'{a["sha256"]}  {name}\n':
                            raise ValueError('checksum sidecar mismatch')
                    found.add(d.name)
                    releases.append(f'{typ}/{platform}/{d.name} ({len(targets)} targets)')
                except (OSError, ValueError, TypeError, KeyError) as e:
                    errors.append(f'{manifest_path}: {e}')
            channel_file = platform_dir / 'latest.json'
            if channel_file.exists():
                try:
                    c = json.loads(channel_file.read_text('utf-8'))
                    if c != {'schemaVersion': 'jani.release-channel/v2', 'type': typ, 'platform': platform, 'latest': c.get('latest')}:
                        raise ValueError('invalid typed channel')
                    if c['latest'] not in found or not VERSION.fullmatch(c['latest']):
                        raise ValueError('latest points to an invalid/unpublished release')
                except (OSError, ValueError, TypeError, KeyError) as e:
                    errors.append(f'{channel_file}: {e}')
    # Previously published v1 releases remain as compatibility aliases. Never create new legacy releases.
    legacy = base / 'latest.json'
    if legacy.exists():
        try:
            c = json.loads(legacy.read_text('utf-8'))
            if c['schemaVersion'] != 'jani.release-channel/v1' or not VERSION.fullmatch(c['latest']) or not (base / c['latest'] / 'manifest.json').is_file():
                raise ValueError('broken legacy release pointer')
        except (OSError, ValueError, TypeError, KeyError) as e:
            errors.append(f'{legacy}: {e}')
    return errors, releases


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('command', nargs='?', choices=('check',), default='check')
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    args = p.parse_args()
    errors, rels = check(args.root)
    for e in errors:
        print('release-catalog:', e, file=sys.stderr)
    for r in rels:
        print('verified:', r)
    if not rels:
        print('release-catalog: no canonical binaries published', file=sys.stderr)
    return int(bool(errors))

if __name__ == '__main__':
    sys.exit(main())
