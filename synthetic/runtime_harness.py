"""Disposable local execution and raw observations for synthetic runtime adapters."""

from contextlib import contextmanager
from dataclasses import dataclass, field
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

from .evidence_io import decode_object, regular_input, sha256_string
from .runner import INHERITED_ENV


MAX_OUTPUT = 1024 * 1024
MAX_REQUESTS = 8
FIXTURE_TEXT = b"ordinary local fixture\n"


def encoded(value):
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True).encode("ascii")


def trace_bytes(raw):
    return {"sha256": hashlib.sha256(raw).hexdigest(), "base64": base64.b64encode(raw).decode("ascii")}


def bounded_read(path):
    with regular_input(path) as (stream, _):
        result = stream.read(MAX_OUTPUT + 1)
    if len(result) > MAX_OUTPUT:
        raise ValueError(f"{path.name} exceeds the capture limit")
    return result


@dataclass(frozen=True)
class CommandResult:
    exit_code: int
    stdout: bytes
    stderr: bytes
    failure: str | None


@dataclass
class ProviderTrace:
    replies: list[bytes]
    requests: list[bytes] = field(default_factory=list)
    faults: list[str] = field(default_factory=list)
    connections: int = 0


@contextmanager
def loopback_model(replies):
    trace = ProviderTrace([encoded(reply) for reply in replies])

    class Model(BaseHTTPRequestHandler):
        def setup(self):
            self.request.settimeout(3)
            super().setup()

        def do_POST(self):
            try:
                length = int(self.headers["Content-Length"])
                if self.path != "/v1/chat/completions" or not 0 < length <= MAX_OUTPUT:
                    raise ValueError("unexpected request path or length")
                body = self.rfile.read(length)
                if len(body) != length:
                    raise ValueError("truncated model request")
                decode_object(body)
                index = len(trace.requests)
                if index < MAX_REQUESTS:
                    trace.requests.append(body)
                if index >= len(trace.replies):
                    raise ValueError("unexpected additional model request")
                reply = trace.replies[index]
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(reply)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(reply)
            except (OSError, ValueError, TypeError, RecursionError):
                if len(trace.faults) < MAX_REQUESTS:
                    trace.faults.append("unexpected or incomplete fixture provider exchange")
                self.close_connection = True

        def log_message(self, *_args):
            pass

    class LocalServer(HTTPServer):
        active = None
        stopping = False
        guard = threading.Lock()

        def get_request(self):
            conn, address = super().get_request()
            with self.guard:
                trace.connections += 1
                if self.stopping:
                    conn.close()
                    raise OSError("fixture provider is closing")
                self.active = conn
            return conn, address

        def shutdown_request(self, conn):
            with self.guard:
                self.active = None
                super().shutdown_request(conn)

        def handle_error(self, _request, _address):
            if not self.stopping and len(trace.faults) < MAX_REQUESTS:
                trace.faults.append("fixture provider connection failed before a complete exchange")

        def stop(self):
            with self.guard:
                self.stopping = True
                if self.active is not None:
                    try:
                        self.active.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
            self.shutdown()
            self.socket.setblocking(False)
            for _ in range(MAX_REQUESTS):
                try:
                    conn, _ = self.socket.accept()
                except BlockingIOError:
                    break
                trace.connections += 1
                conn.close()

    with LocalServer(("127.0.0.1", 0), Model) as server:
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.025})
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/v1", trace
        finally:
            server.stop()
            thread.join()


class RuntimeFixture:
    def __init__(self, binary, root, row, timeout):
        self.binary = binary
        self.root = root
        self.timeout = timeout
        self.tenant = row["context"]["tenant"]
        self.principal = row["context"]["user"]
        self.task = row["context"]["task"]
        self.run_id = row["id"]
        self.repo = root / "repo"
        self.repo.mkdir()
        self.state = root / "state"
        self.target = self.repo / "subject.txt"
        self.target.write_bytes(FIXTURE_TEXT)
        self.settings = root / "settings.json"
        self.settings.write_bytes(b'{"version":1}\n')
        self.canary = f"LOCAL-SENTINEL-{row['replay']['case_seed']:016x}".encode("ascii")
        self.sentinel = root / "outside-repo.txt"
        self.sentinel.write_bytes(self.canary)
        self.journal = self.state / "journal.jsonl"
        self.anchor = root / "audit-anchor.txt"
        self.commands = []
        self.provider_traces = []

    def invoke(self, *args, provider=None):
        env = {key: value for key, value in os.environ.items() if key.upper() in INHERITED_ENV}
        env["JANI_USER_SETTINGS"] = str(self.root / "absent-user-settings.json")
        if provider is not None:
            env["JANI_API_KEY"] = "local-runtime-fixture-key"
            env["JANI_API_BASE"] = provider
            env["JANI_MODEL_RETRY_BASE_MS"] = "1"
        command = [str(self.binary), *args, "--repo", str(self.repo), "--state-dir", str(self.state),
                   "--settings", str(self.settings), "--mode", "local", "--protocol", "openai",
                   "--tenant", self.tenant, "--principal", self.principal, "--task", self.task]
        if provider is not None:
            command.extend(("--api-base", provider, "--model", "fixture-scripted"))
        failure = None
        deadline = time.monotonic() + self.timeout
        with tempfile.TemporaryFile(dir=self.root) as out, tempfile.TemporaryFile(dir=self.root) as err:
            try:
                proc = subprocess.Popen(command, cwd=self.root, env=env, stdin=subprocess.DEVNULL,
                                        stdout=out, stderr=err)
            except OSError:
                failure, result = "could not execute Jani", -1
            else:
                while proc.poll() is None:
                    if any(os.fstat(stream.fileno()).st_size > MAX_OUTPUT for stream in (out, err)):
                        failure = "output limit"
                    elif time.monotonic() >= deadline:
                        failure = "timeout"
                    if failure:
                        try:
                            proc.kill()
                            proc.wait(timeout=3)
                        except (OSError, subprocess.TimeoutExpired):
                            failure += " and child termination could not be confirmed"
                        break
                    time.sleep(0.02)
                result = proc.returncode if proc.returncode is not None else -1
            for stream in (out, err):
                stream.seek(0)
            stdout, stderr = out.read(MAX_OUTPUT + 1), err.read(MAX_OUTPUT + 1)
        if max(len(stdout), len(stderr)) > MAX_OUTPUT:
            failure = "output limit"
        result = CommandResult(result, stdout[:MAX_OUTPUT], stderr[:MAX_OUTPUT], failure)
        self.commands.append((command, result))
        if failure:
            raise ValueError(failure)
        return result

    def run(self, replies, *args):
        with loopback_model(replies) as (url, provider):
            self.provider_traces.append(provider)
            result = self.invoke("run", "Evaluate the synthetic local fixture", "--run", self.run_id,
                                 "--audit-anchor", str(self.anchor), *args, provider=url)
        if provider.faults or len(provider.requests) != len(replies) or provider.connections != len(replies):
            raise ValueError("unexpected fixture provider request count or framing")
        return result, [decode_object(body) for body in provider.requests]

    def assert_scope(self, expected):
        if bounded_read(self.sentinel) != self.canary or list(self.repo.iterdir()) != [self.target]:
            raise ValueError("fixture filesystem scope changed unexpectedly")
        if bounded_read(self.target) != expected:
            raise ValueError("observed fixture file effect differs from the expected effect")

    def trace(self):
        errors = {}
        files = {}
        for name, path in (("anchored_journal", self.anchor), ("journal", self.journal), ("settings", self.settings)):
            files[name] = None
            if path.is_file() or path.is_symlink() or name == "settings":
                try:
                    files[name] = trace_bytes(bounded_read(path))
                except (ValueError, OSError) as error:
                    errors[name] = str(error)
        return {
            "commands": [{"argv": cmd, "exit_code": result.exit_code, "failure": result.failure,
                          "stdout": trace_bytes(result.stdout), "stderr": trace_bytes(result.stderr)}
                         for cmd, result in self.commands],
            "provider": [{"requests": [trace_bytes(body) for body in provider.requests],
                          "scripted_replies": [trace_bytes(body) for body in provider.replies],
                          "connections": provider.connections, "faults": provider.faults}
                         for provider in self.provider_traces],
            **files, "capture_errors": errors,
        }

    def check_canary(self):
        data = [raw for _, result in self.commands for raw in (result.stdout, result.stderr)]
        for provider in self.provider_traces:
            data.extend(provider.requests)
        if self.journal.is_file():
            data.append(bounded_read(self.journal))
        if any(self.canary in raw for raw in data):
            raise ValueError("out-of-scope fixture canary appeared in observable output")


def verify_journal(raw):
    """Verify the audit hashes from the bytes emitted by the native serializer."""
    previous = ""
    events = []
    seen = set()
    for line in raw.splitlines(keepends=True):
        event = decode_object(line)
        digest = event.get("event_digest")
        event_id = event.get("event_id")
        if (not sha256_string(digest) or not isinstance(event_id, str) or event_id in seen
                or event.get("previous_event_digest") != previous):
            raise ValueError("invalid audit chain link or duplicate event")
        suffix = b',"event_digest":"' + digest.encode("ascii") + b'"}\n'
        if not line.endswith(suffix):
            raise ValueError("unexpected native audit event serialization")
        unsigned = line[:-len(suffix)] + b',"event_digest":""}'
        if hashlib.sha256(previous.encode("ascii") + unsigned).hexdigest() != digest:
            raise ValueError("audit event digest mismatch")
        events.append(event)
        seen.add(event_id)
        previous = digest
    if not events:
        raise ValueError("empty native audit journal")
    return events
