"""Cold state backup checks. Set JANI_RECOVERY_DRILL_BIN for the real CLI drill."""

from contextlib import ExitStack, closing
from hashlib import sha256
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

from tools import state_backup as snapshot


def sqlite_rows(path, query, mode="ro"):
    with closing(sqlite3.connect(path.as_uri() + f"?mode={mode}", uri=True)) as db:
        return db.execute(query).fetchall()


def leave_committed_wal(path, script):
    # os._exit skips SQLite close and therefore its final WAL checkpoint.
    program = """
import os, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
assert c.execute('PRAGMA journal_mode=WAL').fetchone() == ('wal',)
c.execute('PRAGMA wal_autocheckpoint=0')
c.executescript(sys.argv[2])
c.commit()
os._exit(0)
"""
    subprocess.run([sys.executable, "-c", program, str(path), script], check=True, timeout=30)
    wal = path.with_name(path.name + "-wal")
    assert wal.stat().st_size > 32
    return wal


class StateBackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="jani-state-backup-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "runtime"
        self.source.mkdir()
        (self.source / "memory.jsonl").write_bytes(b"local fixture\n")
        self.archive = self.root / "backup"
        self.restored = self.root / "restored"

    def backup(self):
        return snapshot.backup(self.source, self.archive)["manifestSha256"]

    def test_round_trip_recovers_uncheckpointed_wal_without_touching_input(self):
        database = self.source / "tasks" / "state.sqlite3"
        database.parent.mkdir()
        wal = leave_committed_wal(database, "CREATE TABLE task_sessions(id, data); INSERT INTO task_sessions VALUES(1, 'last committed row');")
        initial = {p.name: p.read_bytes() for p in database.parent.iterdir()}
        self.assertGreater(len(initial[wal.name]), 32)
        (self.source / "empty").mkdir()
        checkout = self.source / "deliveries" / "test" / "tree"
        checkout.mkdir(parents=True)
        # A project file with a SQLite-like basename is not a runtime database.
        (checkout / "test.sqlite3").write_bytes(b"project fixture")
        (checkout / "test.sqlite3-shm").write_bytes(b"another project fixture")
        if os.name == "posix":
            (checkout / "test.sqlite3").chmod(0o755)
        pin = self.backup()
        self.assertEqual({p.name: p.read_bytes() for p in database.parent.iterdir()}, initial)
        self.assertTrue(snapshot.inspect_backup(self.archive, pin)["verified"])
        state = Path(snapshot.inspect_backup(self.archive, pin, self.restored)["stateDir"])
        self.assertEqual(sqlite_rows(state / "tasks" / database.name, "SELECT * FROM task_sessions"), [(1, "last committed row")])
        self.assertEqual(sqlite_rows(state / "tasks" / database.name, "PRAGMA journal_mode"), [("delete",)])
        self.assertFalse((state / "tasks" / wal.name).exists())
        self.assertFalse((state / "tasks" / f"{database.name}-shm").exists())
        self.assertTrue((state / "empty").is_dir())
        self.assertEqual((state / checkout.relative_to(self.source) / "test.sqlite3-shm").read_bytes(), b"another project fixture")
        self.assertEqual(json.loads((state / snapshot.RESTORE_HOLD_FILE).read_bytes())["manifestSha256"], pin)
        self.assertIsNone(json.loads((state / "kill-switch.json").read_bytes()).get("tenant"))
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(self.archive.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE((state / "memory.jsonl").stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE((state / checkout.relative_to(self.source) / "test.sqlite3").stat().st_mode), 0o700)

    def test_cli_requires_confirmation_and_outputs_a_verifiable_digest(self):
        script = Path(snapshot.__file__).resolve()
        create = [sys.executable, str(script), "backup", "--state-dir", str(self.source), "--out", str(self.archive)]
        out = subprocess.run(create, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("--offline", out.stderr)
        self.assertFalse(self.archive.exists())
        out = subprocess.run([*create, "--offline"], capture_output=True, text=True, check=True, timeout=30)
        pin = json.loads(out.stdout)["manifestSha256"]
        self.assertEqual(sha256((self.archive / "manifest.json").read_bytes()).hexdigest(), pin)
        out = subprocess.run([sys.executable, str(script), "verify", "--backup", str(self.archive), "--manifest-sha256", pin],
                             capture_output=True, text=True, check=True, timeout=30)
        self.assertTrue(json.loads(out.stdout)["verified"])

    def test_wrong_digest_and_existing_destinations_are_refused(self):
        pin = self.backup()
        with self.assertRaisesRegex(snapshot.BackupError, "retained digest"):
            snapshot.inspect_backup(self.archive, "0" * 64, self.restored)
        self.assertFalse(self.restored.exists())
        self.restored.mkdir()
        sentinel = self.restored / "existing"
        sentinel.write_bytes(b"keep")
        with self.assertRaises(FileExistsError):
            snapshot.inspect_backup(self.archive, pin, self.restored)
        with self.assertRaises(FileExistsError):
            snapshot.backup(self.source, self.archive)
        self.assertEqual(sentinel.read_bytes(), b"keep")

    def test_payload_tampering_and_partial_restore_stay_unpublished(self):
        pin = self.backup()
        (self.archive / "state" / "memory.jsonl").write_bytes(b"changed data!\n")
        with self.assertRaisesRegex(snapshot.BackupError, "content differs"):
            snapshot.inspect_backup(self.archive, pin, self.restored)
        self.assertFalse((self.restored / "state").exists())
        (self.archive / "state" / "extra").write_bytes(b"extra")
        with self.assertRaisesRegex(snapshot.BackupError, "paths differ"):
            snapshot.inspect_backup(self.archive, pin)
        (self.archive / "manifest.json").unlink()
        with self.assertRaises(FileNotFoundError):
            snapshot.inspect_backup(self.archive, pin)

    def test_staged_write_corruption_never_publishes_state(self):
        pin = self.backup()
        original = snapshot.stream_entry

        def corrupt(path, info, destination):
            result = original(path, info, destination)
            if destination is not None and destination.name == "memory.jsonl":
                destination.write_bytes(b"staged bytes differ")
            return result

        with patch.object(snapshot, "stream_entry", side_effect=corrupt):
            with self.assertRaisesRegex(snapshot.BackupError, "snapshot content differs"):
                snapshot.inspect_backup(self.archive, pin, self.restored)
        self.assertFalse((self.restored / "state").exists())

    def test_earlier_kill_switch_is_retained_separately(self):
        original = b'{"tenant":"a","actor":"operator","reason":"earlier stop","activatedAt":1}'
        (self.source / "kill-switch.json").write_bytes(original)
        pin = self.backup()
        result = snapshot.inspect_backup(self.archive, pin, self.restored)
        self.assertEqual(Path(result["priorKillSwitch"]).read_bytes(), original)
        self.assertEqual((self.archive / "state" / "kill-switch.json").read_bytes(), original)
        state = Path(result["stateDir"])
        self.assertNotIn("tenant", json.loads((state / "kill-switch.json").read_bytes()))

    def test_nested_destinations_fail_before_creating_output(self):
        with self.assertRaisesRegex(snapshot.BackupError, "outside the input tree"):
            snapshot.backup(self.source, self.source / "nested")
        self.assertFalse((self.source / "nested").exists())
        pin = self.backup()
        for parent in (self.source, self.archive, self.archive / "state"):
            with self.subTest(parent=parent), self.assertRaisesRegex(snapshot.BackupError, "outside the input tree"):
                snapshot.inspect_backup(self.archive, pin, parent / "nested")
            self.assertFalse((parent / "nested").exists())

    def test_changing_input_does_not_seal_snapshot(self):
        original = snapshot.stream_entry

        def mutate(path, info, destination):
            result = original(path, info, destination)
            if path == self.source / "memory.jsonl":
                path.write_bytes(b"writer still running")
            return result

        with patch.object(snapshot, "stream_entry", side_effect=mutate):
            with self.assertRaisesRegex(snapshot.BackupError, "input tree changed"):
                self.backup()
        self.assertFalse((self.archive / "manifest.json").exists())

    def test_hardlinked_entries_are_refused(self):
        os.link(self.source / "memory.jsonl", self.source / "other.jsonl")
        with self.assertRaisesRegex(snapshot.BackupError, "hardlinked"):
            self.backup()
        self.assertFalse(self.archive.exists())

    def test_linked_input_ancestors_and_payload_entries_are_refused(self):
        alias = self.root / "alias"
        try:
            alias.symlink_to(self.source, target_is_directory=True)
        except OSError as error:
            if os.name != "nt" or error.winerror != 1314:
                self.skipTest(f"cannot create symlinks here: {error}")
            subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(alias), str(self.source)],
                           check=True, capture_output=True, timeout=30)
        (self.source / "runtime-child").mkdir()
        with self.assertRaisesRegex(snapshot.BackupError, "link or reparse point"):
            snapshot.backup(alias / "runtime-child", self.archive)
        with self.assertRaisesRegex(snapshot.BackupError, "link or reparse point"):
            snapshot.backup(self.source, alias / "nested")
        if alias.is_symlink():
            (self.source / "link").symlink_to(self.source / "memory.jsonl")
        else:
            subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(self.source / "link"),
                            str(self.source / "runtime-child")], check=True, capture_output=True, timeout=30)
        with self.assertRaisesRegex(snapshot.BackupError, "link or reparse point"):
            self.backup()
        self.assertFalse(self.archive.exists())

    def test_each_runtime_database_with_hot_wal_is_recovered(self):
        stores = {
            "attention/state.sqlite3": "attention_state", "mirror/improvements.sqlite3": "mirror_findings",
            "native/native.sqlite3": "objects", "tasks/state.sqlite3": "task_sessions",
        }
        for name, table in stores.items():
            db = self.source / name
            db.parent.mkdir()
            leave_committed_wal(db, f"CREATE TABLE {table}(value); INSERT INTO {table} VALUES('from hot WAL');")
        report = snapshot.backup(self.source, self.archive)
        self.assertEqual(report["sqliteDatabases"], 4)
        restored = Path(snapshot.inspect_backup(self.archive, report["manifestSha256"], self.restored)["stateDir"])
        for name, table in stores.items():
            self.assertEqual(sqlite_rows(restored / name, f"SELECT value FROM {table}"), [("from hot WAL",)])

    def test_damaged_wal_refuses_success_even_with_valid_main_database(self):
        db = self.source / "tasks" / "state.sqlite3"
        db.parent.mkdir()
        with closing(sqlite3.connect(db)) as connection:
            connection.execute("CREATE TABLE task_sessions(value)")
            connection.execute("INSERT INTO task_sessions VALUES('older')")
            connection.commit()
        wal = leave_committed_wal(db, "INSERT INTO task_sessions VALUES('committed in the WAL');")
        valid = wal.read_bytes()
        for number, (bad, diagnostic) in enumerate([
            (bytes([valid[0] ^ 1]) + valid[1:], "corrupt SQLite WAL header"),
            (valid[:-1] + bytes([valid[-1] ^ 1]), "checksum mismatch"),
            (valid[:-1], "truncated SQLite WAL frame"),
        ]):
            with self.subTest(diagnostic=diagnostic):
                wal.write_bytes(bad)
                output = self.root / f"unsealed-{number}"
                with self.assertRaisesRegex(snapshot.BackupError, diagnostic):
                    snapshot.backup(self.source, output)
                self.assertFalse((output / "manifest.json").exists())

    def test_recycled_wal_with_old_generation_tail_requires_reconciliation(self):
        db = self.source / "tasks" / "state.sqlite3"
        db.parent.mkdir()
        leave_committed_wal(db, """
            PRAGMA journal_size_limit=-1;
            CREATE TABLE task_sessions(value);
            CREATE TABLE fixture_padding(data);
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i < 16)
                INSERT INTO fixture_padding SELECT randomblob(8000) FROM n;
            INSERT INTO task_sessions VALUES('first');
            PRAGMA wal_checkpoint(RESTART);
            UPDATE task_sessions SET value='latest';
        """)
        with self.assertRaisesRegex(snapshot.BackupError, "ambiguous SQLite WAL frame"):
            self.backup()
        self.assertFalse((self.archive / "manifest.json").exists())
        self.assertEqual(sqlite_rows(db, "SELECT value FROM task_sessions"), [("latest",)])

    def test_runtime_database_failure_never_seals_the_archive(self):
        database = self.source / "tasks" / "state.sqlite3"
        database.parent.mkdir()
        database.write_bytes(b"invalid SQLite")
        with self.assertRaises(sqlite3.DatabaseError):
            self.backup()
        self.assertFalse((self.archive / "manifest.json").exists())

    def test_standalone_rollback_journal_and_orphan_wal_refused(self):
        folder = self.source / "tasks"
        folder.mkdir()
        wal = folder / "state.sqlite3-wal"
        wal.write_bytes(b"orphan")
        with self.assertRaisesRegex(snapshot.BackupError, "companion lacks"):
            self.backup()
        wal.unlink()
        (folder / "state.sqlite3").write_bytes(b"placeholder")
        shared = folder / "state.sqlite3-shm"
        shared.write_bytes(b"index without a matching WAL")
        with self.assertRaisesRegex(snapshot.BackupError, "no matching nonempty WAL"):
            self.backup()
        wal.touch()
        with self.assertRaisesRegex(snapshot.BackupError, "no matching nonempty WAL"):
            self.backup()
        wal.unlink()
        shared.unlink()
        (folder / "state.sqlite3-journal").write_bytes(b"rollback journal")
        with self.assertRaisesRegex(snapshot.BackupError, "unexpected rollback journal"):
            self.backup()
        self.assertFalse(self.archive.exists())

    @unittest.skipUnless(os.environ.get("JANI_RECOVERY_DRILL_BIN"), "set JANI_RECOVERY_DRILL_BIN to a built Jani executable")
    def test_native_recovery_drill_preserves_audit_and_queue_and_blocks_execution(self):
        binary = Path(os.environ["JANI_RECOVERY_DRILL_BIN"]).resolve(strict=True)
        repo = self.root / "repo"
        repo.mkdir()
        settings = repo / "settings.json"
        settings.write_text('{"version":1}', encoding="utf-8")
        anchor = self.root / "independent-anchor.txt"
        seen = []

        class Provider(BaseHTTPRequestHandler):
            def do_POST(self):
                seen.append(self.rfile.read(int(self.headers["Content-Length"])))
                payload = b'{"choices":[{"message":{"role":"assistant","content":"local recovery fixture"}}]}'
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        with ThreadingHTTPServer(("127.0.0.1", 0), Provider) as model, ExitStack() as stop:
            serving = threading.Thread(target=model.serve_forever, daemon=True)
            serving.start()
            stop.callback(serving.join)
            stop.callback(model.shutdown)
            env = {key: value for key, value in os.environ.items() if not key.startswith(("JANI_", "OPENAI_"))}
            env.update(JANI_API_BASE=f"http://127.0.0.1:{model.server_port}/v1", JANI_API_KEY="fixture-only", JANI_MODEL="fixture")

            def jani(state, *args, allowed=True):
                out = subprocess.run([str(binary), *args, "--repo", str(repo), "--state-dir", str(state),
                                      "--settings", str(settings)], env=env, capture_output=True, text=True, timeout=30)
                if allowed:
                    self.assertEqual(out.returncode, 0, out.stderr)
                else:
                    self.assertNotEqual(out.returncode, 0, out.stdout)
                    self.assertIn("RESTORE_HOLD_ACTIVE", out.stderr)
                return out.stdout

            jani(self.source, "run", "Seed one fixture checkpoint", "--task", "restore-fixture", "--audit-anchor", str(anchor))
            task = json.loads(jani(self.source, "work", "add", "queued recovery fixture"))
            pinned_head = anchor.read_text(encoding="utf-8").splitlines()[-1].split()[-1]
            db = self.source / "tasks" / "state.sqlite3"
            leave_committed_wal(db, "UPDATE task_sessions SET status='waiting_approval' WHERE task_id='restore-fixture';")
            hold_pin = self.backup()
            restored = Path(snapshot.inspect_backup(self.archive, hold_pin, self.restored)["stateDir"])
            self.assertEqual(sqlite_rows(restored / "tasks" / db.name, "SELECT status FROM task_sessions WHERE task_id='restore-fixture'"), [("waiting_approval",)])
            protected = (self.source, self.archive, restored, repo)

            def contents():
                return [snapshot.tree_entries(tree, snapshot.inventory(tree)) for tree in protected]

            before = contents()
            anchor_before = anchor.read_bytes()
            self.assertEqual(json.loads(jani(restored, "audit", "verify"))["last_digest"], pinned_head)
            docs_form = subprocess.run([str(binary), "audit", "verify", "--repo", "./repo",
                                        "--settings", "settings.json", "--state-dir", "../restored/state"],
                                       cwd=self.root, env=env, capture_output=True, text=True, check=True, timeout=30)
            self.assertEqual(json.loads(docs_form.stdout)["last_digest"], pinned_head)
            recovery = json.loads(jani(restored, "recovery", "status"))
            run = next(run for run in recovery["runs"] if run["taskId"] == "restore-fixture")
            self.assertFalse(run["safeToAutoReplay"])
            self.assertTrue(json.loads(jani(restored, "evidence", run["runId"])))
            self.assertTrue(json.loads(jani(restored, "inspect", run["runId"])))
            self.assertTrue(json.loads(jani(restored, "agent", "kill-switch", "status"))["active"])
            task_record = Path("work", "tasks", f"{task['id']}.json")
            self.assertEqual((restored / task_record).read_bytes(), (self.source / task_record).read_bytes())
            jani(restored, "work", "run", "--once", allowed=False)
            jani(restored, "dispatch", allowed=False)
            jani(restored, "run", "Do not execute", allowed=False)
            jani(restored, "agent", "kill-switch", "clear", allowed=False)
            self.assertEqual(contents(), before)
            self.assertEqual(anchor.read_bytes(), anchor_before)
            self.assertEqual(len(seen), 1)


if __name__ == "__main__":
    unittest.main()
