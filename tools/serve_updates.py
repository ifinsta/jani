#!/usr/bin/env python3
"""Serve the typed Jani release tree as an update origin.

The updater fetches:

    <origin>/agent/<platform>/latest.json
    <origin>/agent/<platform>/<tag>/manifest.json
    <origin>/agent/<platform>/<tag>/<binary>

Point --root at the repository's releases/ directory, or at a copy that an
operator publishes to. Publishing a channel is a two-step operation: first add
the version directory, then atomically replace latest.json. The updater never
follows redirects and rejects non-loopback plain HTTP origins.

Usage:
    python tools/serve_updates.py --port 8787
    python tools/serve_updates.py --root releases --host 0.0.0.0 --port 8787
"""

import argparse
import functools
import http.server
import json
import os
import sys


class ReleaseHandler(http.server.SimpleHTTPRequestHandler):
    server_version = "jani-release-origin/1"

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def list_directory(self, path):
        self.send_error(404, "directory listing disabled")
        return None

    def log_message(self, fmt, *args):
        sys.stderr.write("origin: " + (fmt % args) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        default=os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "releases"),
        help="directory containing agent/<platform>/ channel trees (default: ./releases)",
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8787)
    args = parser.parse_args()

    root = os.path.abspath(args.root)
    if not os.path.isdir(root):
        parser.error(f"release root does not exist: {root}")

    handler = functools.partial(ReleaseHandler, directory=root)
    server = http.server.ThreadingHTTPServer((args.host, args.port), handler)
    host, port = server.server_address[:2]
    print(
        json.dumps(
            {
                "schemaVersion": "jani.update-origin/v1",
                "url": f"http://{host}:{port}",
                "root": root,
            }
        ),
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
