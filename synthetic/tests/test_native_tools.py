import tempfile, unittest
from pathlib import Path
from synthetic.native_tools import TOOLS,FAULTS,PROFILES,MUTATIONS,case,generate,verify

class NativeFaultCorpusTests(unittest.TestCase):
 def test_every_native_tool_and_fault_is_represented(self):
  seen=set();families=set()
  for i in range(1,12*20*2+1):
   row=case(i)
   families.add(row['tool']);seen.add(row['tool']+'.'+row['fault'])
   self.assertEqual(row['execution']['status'],'NOT_RUN')
   self.assertTrue(row['oracle']['must_never_bypass_policy'])
  self.assertEqual(families,set(TOOLS))
  self.assertEqual(len(seen),sum(map(len,FAULTS.values())))
 def test_deterministic_replay_and_tenant_denial(self):
  for i in (1,12,13,1001,100001,240000):
   self.assertEqual(case(i,20261025),case(i,20261025))
   self.assertNotEqual(case(i,20261025),case(i,20261026))
  for i in range(19,19000,19):
   row=case(i)
   self.assertNotEqual(row['tenant'],row['resource_tenant'])
   self.assertEqual(row['oracle']['expected'],'REFUSE')
 def test_generate_validate_and_detect_corruption(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp)/'cases'
   manifest=generate(root, count=2400, shard_size=400)
   self.assertEqual(manifest['records'],2400)
   self.assertTrue(verify(root)['verified'])
   self.assertEqual(verify(root)['executed'],0)
   with (root/manifest['shards'][0]['file']).open('ab') as f:f.write(b'corruption')
   with self.assertRaises(ValueError):verify(root)
 def test_all_expected_tools_have_source_modules(self):
  self.assertEqual(len(TOOLS),12)
  self.assertEqual(len(PROFILES),16)
  self.assertEqual(len(MUTATIONS),16)

if __name__=='__main__':unittest.main()
