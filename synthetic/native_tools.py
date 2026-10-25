"""Deterministic fault scenarios for 12 native Jani tools. No external dependencies.

All records are UNEXECUTED plans; verification proves corpus integrity only.
"""
from __future__ import annotations
import argparse,gzip,hashlib,json
from pathlib import Path

TOOLS=("index","context","planner","pruner","checkpoint","verifier",
       "impact","scheduler","workspace","evidence","optimizer","simulator")
FAULTS={
 "index":"stale_hash symlink_directory protected_filename unreadable_file huge_file binary_utf8 malicious_path concurrent_edit stale_deleted_file hash_collision_claim overlapping_scan case_fold_duplicate unicode_filename unindexed_source index_cross_tenant index_disk_full index_partial_snapshot index_budget_exhaustion index_rebuild_race source_secret_leak".split(),
 "context":"token_budget_too_small pinned_over_budget pin_unindexed_path stale_file_digest secret_in_snippet cross_tenant_cache duplicate_hits lost_pins oversized_symbols unavailable_index ranking_bias context_after_revocation source_rename unsanitized_result context_overflow wrong_task_context summarization_hallucination stale_source_reference context_injection token_estimate_inaccurate".split(),
 "planner":"cyclic_dag missing_dependency duplicate_node malformed_json invalid_transition no_acceptance_criteria scope_expansion dangerous_tool_plan plan_overwrite plan_after_revocation orphaned_node unsupported_phase overlarge_graph nonterminating_dependencies stale_evidence completion_without_receipt cross_tenant_plan hidden_dependency policy_changed_after_plan incompatible_task_schema".split(),
 "pruner":"prune_pinned_evidence delete_audit_log remove_live_checkpoint delete_unverified_claim delete_source_file prune_wrong_tenant stale_cache_reference interrupted_pruning quota_race duplicate_entry prune_old_policy remove_human_approval prune_nonexpired_memory prune_during_execution path_traversal prune_dry_run_side_effect bad_retention_age prune_unbacked_state delete_task_graph accidental_vacuum".split(),
 "checkpoint":"corrupt_digest missing_checkpoint stale_plan_after_save interrupted_checkpoint old_revision_replay restored_other_tenant lost_model_state false_model_resume external_effect_replay binary_in_snapshot checkpoint_overwrite symlinked_store clock_rollback unauthorized_checkpoint checkpoint_deadline_expired invalid_node_transition missing_audit_receipt inconsistent_task_identity partial_recovery crash_during_write".split(),
 "verifier":"unapproved_test_execution fake_test_pass unsupported_runner unit_only_claim flaky_test unsupported_platform wrong_workspace_tests malicious_test_script expired_test_receipt stale_source_test changed_code_after_test failure_masked_by_exit_zero skipped_test_as_pass bad_command_argv test_timeout output_truncation dropped_failure partial_test_log inconsistent_ci_status empty_test_selection".split(),
 "impact":"missing_index module_rename changed_public_contract incomplete_dependents dynamic_imports reflection_usage dependency_cycle wrong_source_case new_module_untracked test_map_missing dependency_shadow stale_sha incorrect_symbol_resolution unsafe_absolute_path unknown_file bad_glob_match transitive_change_ignored cross_package_dep protocol_compatibility missing_reverse_edge".split(),
 "scheduler":"blocked_dependency_run incomplete_task_as_done priority_inversion starving_ready_task expired_budget ignored_deadline concurrent_claim_double_execution quarantined_agent_scheduled signed_policy_changed wrong_tenant_owner retry_nonidempotent unknown_task task_graph_cycle scheduler_wake_storm human_commit_scheduled missing_acceptance deadlock starvation_timeout stale_snapshot manual_override".split(),
 "workspace":"symlink_escape hardlink_alias ignored_untracked_diff source_modified_after_snapshot stale_git_head partial_write overwrite_outside_scope wrong_tenant_backup silent_rollback malicious_git_hook deleted_source_not_detected untracked_source_missing path_confusable Windows_UNC_case sensitive_name_mismatch protected_secret_read unsafe_restore overwritten_staged_file index_snapshot_mismatch untracked_backup_leak".split(),
 "evidence":"forged_receipt source_path_missing hash_mismatch altered_file after_verification replayed_source_ref raw_secret_in_receipt untrusted_reporting_agent cross_tenant_evidence false_approval_claim nondurable_record duplicate_id evidence_file_symlink orphaned_evidence provenance_lost wrong_commit_digest expired_receipt mismatched_task unsigned_attestation metric_forgery".split(),
 "optimizer":"unverified_journal invalid_chain cross_tenant_analysis speculative_root_cause autoapply_recommendation wrong_event_time skewed_cost_metric cherry_picked_runs cache_misleading false_success bad_comparison duplicated_findings recurring_denial_hidden history_truncation unbounded_model_reflection unsafe_policy_proposal stale_recommendation poisoned_audit_prompt unproven_efficiency_gain agent_self_certification".split(),
 "simulator":"simulation_claimed_real side_effect_executed nondeterministic_seed missing_fault_hook infinite_loop hidden_test_result invalid_task_graph cost_not_measured token_not_measured simulated_receipt_signed user_commit_simulation_bypass cross_tenant_test_data stress_oom clock_jump chaotic_retry nonreproducible_trace faulty_oracle false_negative false_positive recovery_without_checkpoint".split(),
}
SOURCE={
 "index":"backend/native/src/repo.rs","context":"backend/native/src/memory.rs",
 "planner":"backend/native/src/plans.rs","pruner":"backend/native/src/memory.rs",
 "checkpoint":"backend/native/src/plans.rs","verifier":"backend/native/src/repo.rs",
 "impact":"backend/native/src/repo.rs","scheduler":"backend/native/src/plans.rs",
 "workspace":"backend/native/src/repo.rs","evidence":"backend/native/src/memory.rs",
 "optimizer":"backend/native/src/memory.rs","simulator":"backend/native/src/plans.rs",
}
PROFILES=("unmanaged","enterprise","read_only","cross_tenant","strict_policy","low_budget",
          "no_index","corrupt_storage","crashed_worker","offline","windows","linux","macos",
          "high_concurrency","large_repository","secret_heavy")
MUTATIONS=("base","zero","boundary","overflow","unicode","duplicate","stale","race","crash",
           "tamper","cross_scope","expired","missing","malformed","concurrent","replay")
EXPECTED=("REFUSE","QUARANTINE","SKIP","WAIT_FOR_APPROVAL","INVALIDATE","RECONCILE")
MASK=(1<<64)-1
def mix(n:int)->int:
 n=(n+0x9e3779b97f4a7c15)&MASK
 n=((n^(n>>30))*0xbf58476d1ce4e5b9)&MASK
 n=((n^(n>>27))*0x94d049bb133111eb)&MASK
 return n^(n>>31)
def case(i:int,seed:int=20261025):
 if i<1:raise ValueError("case index must be >= 1")
 x=i-1
 tool=TOOLS[x%len(TOOLS)]
 epoch=x//len(TOOLS)
 fault=FAULTS[tool][epoch%len(FAULTS[tool])]
 variant=MUTATIONS[(epoch//len(FAULTS[tool]))%len(MUTATIONS)]
 rand=mix(i^seed)
 profile=PROFILES[(epoch//(len(FAULTS[tool])*len(MUTATIONS))+rand%len(PROFILES))%len(PROFILES)]
 secondary=(FAULTS[TOOLS[(rand>>8)%len(TOOLS)]][(rand>>21)%20] if i%13==0 else None)
 tenant=f"org-{1+rand%97:03d}"
 target=(f"org-{1+(rand+1)%97:03d}" if i%19==0 else tenant)
 expected=("REFUSE" if profile in ("read_only","cross_tenant","strict_policy") or target!=tenant else
           "RECONCILE" if "crash" in fault or variant=="crash" else
           "WAIT_FOR_APPROVAL" if "approval" in fault else
           "INVALIDATE" if "stale" in fault or "changed" in fault else
           "QUARANTINE" if "secret" in fault or "forged" in fault else
           "SKIP" if "unsupported" in fault else "REFUSE")
 return {"id":f"NATIVE-{i:010d}","seed":seed,"tool":tool,"source":SOURCE[tool],
    "fault":fault,"secondary_fault":secondary,"mutation":variant,"profile":profile,
    "principal":f"human-{1+(rand>>7)%1009:04d}","tenant":tenant,
    "resource_tenant":target,"task":f"task-{1+(rand>>19)%13007:05d}",
    "parameters":{"files":int((rand>>17)%15001),"tokens":int((rand>>23)%32001),
        "concurrency":1+int((rand>>29)%1024),"elapsed_ms":int((rand>>37)%900000),
        "disk_pressure":(rand>>49)%9==0},
    "oracle":{"expected":expected,"must_never_bypass_policy":True,
        "must_never_claim_untested_success":True,"no_external_side_effect":True},
    "execution":{"status":"NOT_RUN","adapter_required":True}}
def generate(root:Path,count:int=240_000,shard_size:int=20_000,seed:int=20261025):
 if not 1<=count<=5_000_000 or not 1<=shard_size<=100_000:raise ValueError("invalid count/shard size")
 root=Path(root);root.mkdir(parents=True,exist_ok=True)
 shards=root/"shards";shards.mkdir(exist_ok=True)
 if list(shards.iterdir()) or (root/"manifest.json").exists():raise ValueError("output already contains shards")
 manifest={"schema":"jani.native.synthetic/v1","records":count,"seed":seed,
    "shard_size":shard_size,"executed":0,"shards":[],"production_ready":False}
 counts={tool:0 for tool in TOOLS};faults=set()
 for start in range(1,count+1,shard_size):
    end=min(count,start+shard_size-1)
    name=f"native_{start:010d}_{end:010d}.jsonl.gz";path=shards/name
    with path.open("wb") as raw:
      with gzip.GzipFile(fileobj=raw,mode="wb",filename="",mtime=0,compresslevel=1) as gz:
        for i in range(start,end+1):
          row=case(i,seed);counts[row["tool"]]+=1;faults.add(row["tool"]+"."+row["fault"])
          gz.write((json.dumps(row,separators=(",",":"),sort_keys=True)+"\n").encode())
    with path.open("rb") as f:sha=hashlib.file_digest(f,"sha256").hexdigest()
    manifest["shards"].append({"file":"shards/"+name,"rows":end-start+1,
        "first":start,"last":end,"sha256":sha,"bytes":path.stat().st_size})
 (root/"manifest.json").write_text(json.dumps(manifest,indent=2,sort_keys=True)+"\n")
 (root/"coverage.json").write_text(json.dumps({"by_tool":counts,"primary_faults":len(faults),
   "mutations":len(MUTATIONS),"profiles":len(PROFILES),
   "disclaimer":"All cases are generated, not executed."},indent=2,sort_keys=True)+"\n")
 return manifest
def verify(root:Path,full=True):
 root=Path(root);m=json.loads((root/"manifest.json").read_text());count=0
 for sh in m["shards"]:
  path=root/sh["file"]
  with path.open("rb") as f:sha=hashlib.file_digest(f,"sha256").hexdigest()
  if sha!=sh["sha256"]:raise ValueError("checksum mismatch: "+sh["file"])
  if full:
   with gzip.open(path,"rt") as f:
    for offset,line in enumerate(f):
     if json.loads(line)!=case(sh["first"]+offset,m["seed"]):raise ValueError("case mismatch")
    if offset+1!=sh["rows"]:raise ValueError("row count mismatch")
  count+=sh["rows"]
 if count!=m["records"]:raise ValueError("manifest count mismatch")
 return {"verified":True,"records":count,"shards":len(m["shards"]),"full":full,"executed":0}
def main():
 p=argparse.ArgumentParser();sub=p.add_subparsers(dest="command",required=True)
 g=sub.add_parser("generate");g.add_argument("--output",type=Path,required=True)
 g.add_argument("--count",type=int,default=240_000);g.add_argument("--shard-size",type=int,default=20_000)
 g.add_argument("--seed",type=int,default=20261025)
 v=sub.add_parser("verify");v.add_argument("--output",type=Path,required=True);v.add_argument("--checksums-only",action="store_true")
 s=sub.add_parser("sample");s.add_argument("--count",type=int,default=12)
 a=p.parse_args()
 if a.command=="generate":print(json.dumps(generate(a.output,a.count,a.shard_size,a.seed)["shards"][-1]))
 elif a.command=="verify":print(json.dumps(verify(a.output,not a.checksums_only)))
 else:
  for i in range(1,a.count+1):print(json.dumps(case(i)))
if __name__=="__main__":main()
