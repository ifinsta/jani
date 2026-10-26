#!/usr/bin/env python3
"""Drive a long Jani REPL session against a deliberately unreliable provider.

The harness proves continuity of one live `jani repl` process across many model
turns while the provider drops connections, returns 429/503, truncates JSON and
occasionally answers slowly. Failure streaks are capped below the client's retry
ceiling so a recovered turn is expected; any lost turn fails the run.

`--simulated-interval-seconds` compresses time: the report states the equivalent
duration but never claims that duration was actually elapsed. Pass
`--pace-seconds` to run in real time instead (for a real multi-day soak).
"""
import argparse
import hashlib
import json
import os
import random
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

TOKEN = re.compile(r"SOAK-[A-Za-z0-9-]+")
ROOT = Path(__file__).resolve().parents[1]


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = 0
        self.injected = {"http503": 0, "http429": 0, "dropped": 0, "truncated": 0}
        self.recovered = 0
        self.streak = 0
        self.max_streak = 0
        self.tool_calls = 0
        self.failures_since_success = 0

    def snapshot(self):
        with self.lock:
            return {
                "modelRequests": self.requests,
                "injectedFailures": dict(self.injected),
                "recoveredFailures": self.recovered,
                "maxConsecutiveFailuresObserved": self.max_streak,
                "toolCallsServed": self.tool_calls,
            }


class Provider:
    def __init__(self, failure_rate, max_consecutive, seed, tool_every, latency_ms):
        self.rng = random.Random(seed)
        self.failure_rate = failure_rate
        self.max_consecutive = max_consecutive
        self.tool_every = tool_every
        self.latency_ms = latency_ms
        self.stats = Stats()
        self.successes = 0
        provider = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_POST(self):
                length = int(self.headers.get("content-length", "0") or 0)
                body = self.rfile.read(length) if length else b"{}"
                provider.handle(self, body)

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base(self):
        return f"http://127.0.0.1:{self.port}/v1"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=10)

    def snapshot(self):
        return self.stats.snapshot()

    def _decide_failure(self):
        with self.stats.lock:
            self.stats.requests += 1
            if (
                self.stats.streak < self.max_consecutive
                and self.rng.random() < self.failure_rate
            ):
                self.stats.streak += 1
                self.stats.max_streak = max(self.stats.max_streak, self.stats.streak)
                self.stats.failures_since_success += 1
                kind = self.rng.choice(
                    ["http503", "http429", "dropped", "truncated"]
                )
                self.stats.injected[kind] += 1
                return kind
            if self.stats.failures_since_success:
                self.stats.recovered += self.stats.failures_since_success
                self.stats.failures_since_success = 0
            self.stats.streak = 0
            return None

    def handle(self, handler, raw):
        failure = self._decide_failure()
        if failure == "dropped":
            try:
                handler.connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            handler.close_connection = True
            return
        if failure in ("http503", "http429"):
            status = 503 if failure == "http503" else 429
            body = f"provider unavailable ({status})".encode()
            self._send(handler, status, body)
            return
        if failure == "truncated":
            self._send(handler, 200, b'{"choices": [')
            return
        if self.latency_ms:
            time.sleep(self.rng.uniform(0, self.latency_ms) / 1000.0)
        try:
            request = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._send(handler, 200, b'{"error": "bad request"}')
            return
        messages = request.get("messages") or []
        token = ""
        for message in messages:
            content = message.get("content")
            if isinstance(content, str):
                found = TOKEN.findall(content)
                if found:
                    token = found[-1]
        last_role = messages[-1].get("role") if messages else ""
        if self.tool_every and last_role != "tool" and self.successes % self.tool_every == 0:
            self.successes += 1
            with self.stats.lock:
                self.stats.tool_calls += 1
            self._send(handler, 200, json.dumps(self._tool_call()).encode())
            return
        if last_role != "tool":
            self.successes += 1
        self._send(handler, 200, json.dumps(self._answer(token)).encode())

    def _send(self, handler, status, body):
        try:
            handler.send_response(status)
            handler.send_header("content-type", "application/json")
            handler.send_header("content-length", str(len(body)))
            handler.send_header("connection", "close")
            handler.end_headers()
            handler.wfile.write(body)
            handler.wfile.flush()
        except OSError:
            pass
        handler.close_connection = True

    def _answer(self, token):
        text = f"ack {token}" if token else "ack"
        return {
            "choices": [{"message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3},
        }

    def _tool_call(self):
        return {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [
                            {
                                "id": "call-soak-tool",
                                "type": "function",
                                "function": {
                                    "name": "workspace_read",
                                    "arguments": json.dumps({"path": "src/one.txt"}),
                                },
                            }
                        ],
                    }
                }
            ],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3},
        }


class Reader(threading.Thread):
    def __init__(self, stream):
        super().__init__(daemon=True)
        self.stream = stream
        self.lock = threading.Lock()
        self.text = ""
        self.finished = False

    def run(self):
        try:
            while True:
                chunk = self.stream.readline()
                if not chunk:
                    break
                with self.lock:
                    self.text += chunk.decode("utf-8", "replace")
        finally:
            with self.lock:
                self.finished = True

    def contains(self, needle):
        with self.lock:
            return needle in self.text

    def snapshot(self):
        with self.lock:
            return self.text


def wait_for(reader, needle, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if reader.contains(needle):
            return True
        if reader.finished:
            return reader.contains(needle)
        time.sleep(0.01)
    return False


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def journal_counts(state_dir):
    journal = state_dir / "journal.jsonl"
    completed = failed = tool_reads = 0
    if not journal.is_file():
        return {"completedRuns": 0, "failedRuns": 0, "toolReads": 0}
    with journal.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            event = json.loads(line)
            if event.get("action") == "agent.run" and event.get("decision") == "completed":
                completed += 1
            if event.get("action") == "agent.run" and event.get("decision") == "failed":
                failed += 1
            if event.get("action") == "repo.read" and event.get("decision") == "executed":
                tool_reads += 1
    return {"completedRuns": completed, "failedRuns": failed, "toolReads": tool_reads}


def audit_ok(binary, state_dir, env):
    result = subprocess.run(
        [str(binary), "audit", "verify", "--state-dir", str(state_dir)],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
    )
    if result.returncode != 0:
        return False, result.stderr.strip() or result.stdout.strip()
    try:
        return bool(json.loads(result.stdout).get("ok")), ""
    except json.JSONDecodeError:
        return False, "audit verify did not return JSON"


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="jani binary (defaults to target/debug)")
    parser.add_argument("--turns", type=int, default=200)
    parser.add_argument("--failure-rate", type=float, default=0.25)
    parser.add_argument("--max-consecutive-failures", type=int, default=3)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--tool-every", type=int, default=0,
                        help="serve a workspace_read tool call every Nth successful turn")
    parser.add_argument("--latency-ms", type=int, default=0,
                        help="upper bound of simulated provider latency on successful replies")
    parser.add_argument("--simulated-interval-seconds", type=float, default=90.0)
    parser.add_argument("--pace-seconds", type=float, default=0.0,
                        help="real sleep between turns; >0 marks the run as real-time")
    parser.add_argument("--turn-timeout", type=float, default=90.0)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--workdir", type=Path)
    parser.add_argument("--keep-state", action="store_true")
    return parser.parse_args()


def main():
    args = parse_args()
    if args.turns < 1:
        print("soak: --turns must be positive", file=sys.stderr)
        return 2
    if not 0.0 <= args.failure_rate <= 1.0:
        print("soak: --failure-rate must be between 0 and 1", file=sys.stderr)
        return 2
    binary = (args.binary or ROOT / "target/debug/jani.exe").resolve()
    if os.name != "nt":
        candidate = ROOT / "target/debug/jani"
        binary = (args.binary or candidate).resolve()
    if not binary.is_file():
        print(f"soak: binary not found: {binary}", file=sys.stderr)
        return 2

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    workdir = args.workdir or Path(tempfile.mkdtemp(prefix=f"jani-soak-{stamp}-"))
    workdir.mkdir(parents=True, exist_ok=True)
    repo = workdir / "repo"
    (repo / "src").mkdir(parents=True, exist_ok=True)
    (repo / "src/one.txt").write_text("soak fixture\n", encoding="utf-8")
    state = workdir / "state"
    state.mkdir(parents=True, exist_ok=True)
    report_path = args.report or ROOT / "validation" / "soak" / f"{stamp}-soak.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    for key in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "JANI_SETTINGS",
                "JANI_POLICY", "JANI_IDENTITY", "JANI_PROTOCOL"):
        env.pop(key, None)
    provider = Provider(
        args.failure_rate,
        args.max_consecutive_failures,
        args.seed,
        args.tool_every,
        args.latency_ms,
    )
    env.update(
        JANI_API_KEY="soak-only-not-a-real-credential",
        JANI_API_BASE=provider.base,
        JANI_MODEL="soak-model",
        JANI_MODEL_RETRY_BASE_MS="2",
        JANI_USER_SETTINGS=str(workdir / "no-user-settings.json"),
    )

    version = subprocess.run(
        [str(binary), "--version"], capture_output=True, text=True, env=env, timeout=60
    ).stdout.strip()
    started = time.monotonic()
    started_at = datetime.now(timezone.utc).isoformat()
    child = subprocess.Popen(
        [
            str(binary),
            "repl",
            "--repo", str(repo),
            "--state-dir", str(state),
            "--max-steps", "4",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    stdout = Reader(child.stdout)
    stderr = Reader(child.stderr)
    stdout.start()
    stderr.start()

    session = f"{args.seed}-{stamp}"
    turns_completed = 0
    lost_turn = None
    error = None
    try:
        for index in range(1, args.turns + 1):
            token = f"SOAK-{session}-{index}"
            if child.poll() is not None:
                error = f"repl exited before turn {index} with {child.returncode}"
                break
            child.stdin.write(
                f"Turn {index}. Reply with token {token}.\n".encode()
            )
            child.stdin.flush()
            if not wait_for(stdout, token, args.turn_timeout):
                lost_turn = index
                error = f"turn {index} produced no answer within {args.turn_timeout}s"
                break
            turns_completed += 1
            if args.pace_seconds:
                time.sleep(args.pace_seconds)
        if error is None:
            child.stdin.write(b"/exit\n")
            child.stdin.flush()
        child.stdin.close()
    except BrokenPipeError:
        error = "repl closed its input before the soak finished"
    finally:
        try:
            child.wait(timeout=60)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        stdout.join(timeout=5)
        stderr.join(timeout=5)
        provider.close()

    elapsed = time.monotonic() - started
    finished_at = datetime.now(timezone.utc).isoformat()
    counts = journal_counts(state)
    audit_ok_value, audit_error = audit_ok(binary, state, env)
    stderr_text = stderr.snapshot()
    if error is None and child.returncode != 0:
        error = f"repl exited with {child.returncode}"
    if error is None and turns_completed != args.turns:
        error = f"only {turns_completed} of {args.turns} turns completed"
    if error is None and counts["completedRuns"] != args.turns:
        error = (
            f"journal records {counts['completedRuns']} completed runs for "
            f"{args.turns} turns"
        )
    if error is None and counts["failedRuns"]:
        error = f"journal records {counts['failedRuns']} failed runs"
    if error is None and not audit_ok_value:
        error = f"journal integrity verification failed: {audit_error}"
    if error is None and args.tool_every and counts["toolReads"] == 0:
        error = "tool-call turns were requested but no successful repo.read was recorded"

    stats = provider.snapshot()
    equivalent_hours = (
        turns_completed * args.simulated_interval_seconds / 3600.0
        if not args.pace_seconds
        else elapsed / 3600.0
    )
    report = {
        "schemaVersion": "jani.soak/v1",
        "verdict": "pass" if error is None else "fail",
        "error": error,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "elapsedSeconds": round(elapsed, 3),
        "mode": "real-time" if args.pace_seconds else "accelerated",
        "simulatedIntervalSeconds": args.simulated_interval_seconds,
        "equivalentHours": round(equivalent_hours, 2),
        "equivalentDays": round(equivalent_hours / 24.0, 2),
        "turnsRequested": args.turns,
        "turnsCompleted": turns_completed,
        "lostTurn": lost_turn,
        "provider": {
            "failureRate": args.failure_rate,
            "maxConsecutiveFailures": args.max_consecutive_failures,
            "seed": args.seed,
            "toolEvery": args.tool_every,
            "latencyMs": args.latency_ms,
            **stats,
        },
        "journal": {
            **counts,
            "auditOk": audit_ok_value,
        },
        "binary": {
            "path": str(binary),
            "version": version,
            "sha256": sha256(binary),
        },
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print(f"soak: {report['verdict'].upper()} after {turns_completed}/{args.turns} turns "
          f"in {elapsed:.1f}s ({report['mode']}, ~{report['equivalentHours']}h equivalent)")
    print(f"soak: {stats['modelRequests']} model requests, "
          f"{sum(stats['injectedFailures'].values())} injected faults, "
          f"{stats['recoveredFailures']} recovered, max streak "
          f"{stats['maxConsecutiveFailuresObserved']}")
    print(f"soak: journal {counts['completedRuns']} completed / {counts['failedRuns']} failed; "
          f"audit {'ok' if audit_ok_value else 'FAILED'}; report {report_path}")
    if error:
        print(f"soak: {error}", file=sys.stderr)
        if stderr_text.strip():
            print(stderr_text[-2000:], file=sys.stderr)
    if not args.keep_state and args.workdir is None:
        shutil.rmtree(workdir, ignore_errors=True)
    return 0 if error is None else 1


if __name__ == "__main__":
    sys.exit(main())
