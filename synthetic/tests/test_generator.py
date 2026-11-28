import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from synthetic.catalog import FAULTS, PROFILES, VARIANTS
from synthetic.generate import case,generate,verify
from synthetic.readiness import evaluate
class TestCorpus(unittest.TestCase):
    def test_catalog_and_determinism(self):
        self.assertGreaterEqual(len(FAULTS),20)
        self.assertGreaterEqual(sum(map(len,FAULTS.values())),300)
        for i in (1,23,99,1000,999999,5_000_000):
            self.assertEqual(case(i,123),case(i,123))
            self.assertNotEqual(case(i,123)['replay']['case_seed'],case(i,456)['replay']['case_seed'])
    def test_full_small_corpus_and_corruption(self):
        with tempfile.TemporaryDirectory() as t:
            out=Path(t)/'run';m=generate(out,count=1203,shard_size=99,quiet=True)
            self.assertEqual(m['records'],1203)
            verify(out)
            self.assertFalse(evaluate(out)['ready'])
            with (out/m['shards'][0]['file']).open('ab') as f:f.write(b'garbage')
            with self.assertRaises(ValueError):verify(out)
    def test_cross_tenant_is_denied(self):
        for i in range(13,10000,13):
            c=case(i)
            if c['context']['tenant']!=c['context']['source_tenant']:
                self.assertEqual(c['oracle']['decision'],'DENY')
    def test_all_faults_seen_in_bounded_cycle(self):
        seen=set()
        for i in range(1,len(FAULTS)*30+1):
            c=case(i);seen.add((c['domain'],c['fault']))
        self.assertEqual(len(seen),sum(map(len,FAULTS.values())))
if __name__=='__main__':unittest.main()

class TestProbe(unittest.TestCase):
    def test_unknown_hooks_are_never_counted_passed(self):
        from synthetic.runner import run_probe
        row=run_probe(Path('/bin/false'),1)
        self.assertEqual(row['status'],'SKIPPED')

    def test_cli_probe_uses_the_correct_phase_and_a_fresh_settings_path(self):
        from synthetic.runner import run_probe

        index = 12533  # First baseline missing-argument fixture without compound faults.
        with patch.dict(os.environ, {"PROBE_FAKE_CREDENTIAL": "ignored", "JANI_USER_SETTINGS": "outside.json"}), \
                patch("synthetic.runner.subprocess.run") as process:
            process.return_value = subprocess.CompletedProcess([], 1, b"", b"AUTH_CONTEXT_MISSING")
            self.assertEqual(run_probe(Path("unused"), index)["status"], "FAILED")
            args, kwargs = process.call_args
            self.assertEqual(args[0][1:3], ["plugins", "inspect"])
            self.assertEqual(Path(kwargs["env"]["JANI_USER_SETTINGS"]).parent, Path(kwargs["cwd"]))
            self.assertNotIn("PROBE_FAKE_CREDENTIAL", kwargs["env"])

            process.return_value = subprocess.CompletedProcess([], 1, b"", b"plugins inspect requires a plugin id")
            seen = set()
            result = run_probe(Path("unused"), index, seen=seen)
            self.assertEqual((result["status"], result["execution"]), ("PASSED", "preflight"))
            self.assertEqual(run_probe(Path("unused"), index, seen=seen)["reason"], "PREFLIGHT_ALREADY_RUN")
            self.assertEqual(process.call_count, 2)

    def test_failed_preflight_exits_nonzero_and_preserves_the_report(self):
        with tempfile.TemporaryDirectory() as folder:
            report = Path(folder) / "report.jsonl"
            args = [sys.executable, "-m", "synthetic.runner", "--binary", sys.executable,
                    "--limit", "1000", "--output", str(report)]
            process = subprocess.run(args, capture_output=True, text=True, timeout=15)
            self.assertEqual(process.returncode, 1, process.stderr)
            summary = json.loads(process.stdout)
            self.assertEqual(summary["execution"], "preflight")
            self.assertGreater(summary["results"]["FAILED"], 0)
            original = report.read_bytes()
            duplicate = subprocess.run(args, capture_output=True, text=True, timeout=15)
            self.assertNotEqual(duplicate.returncode, 0)
            self.assertEqual(report.read_bytes(), original)
