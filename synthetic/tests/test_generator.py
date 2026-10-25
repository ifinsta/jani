import tempfile
import unittest
from pathlib import Path
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
