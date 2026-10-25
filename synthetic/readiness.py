"""Production readiness evidence gate. Never infer readiness from fixture count."""
import argparse, json
from pathlib import Path
MANDATORY = (
    'rust_workspace_tests', 'linux_security_tests', 'windows_security_tests',
    'mcp_source_conformance', 'sandbox_isolation', 'crash_recovery',
    'data_governance_egress', 'authz_revocation', 'human_only_commit',
    'no_side_effect_replay', 'audit_integrity', 'budget_and_approval',
    'synthetic_oracle_execution', 'load_soak_latency', 'operator_disaster_recovery',
    'docs_claims',
)
def evaluate(corpus:Path,receipts:Path|None=None):
    manifest=json.loads((corpus/'manifest.json').read_text())
    evidence=json.loads(receipts.read_text()) if receipts and receipts.exists() else {}
    gates={k:bool(evidence.get(k,{}).get('passed') is True and
                  evidence.get(k,{}).get('artifact_sha256') and
                  evidence.get(k,{}).get('source_commit')) for k in MANDATORY}
    observed=int(evidence.get('executed_cases',0))
    coverage_ok=observed>=manifest['records'] and bool(evidence.get('oracle_passed') is True)
    passed=all(gates.values()) and coverage_ok
    return {'schema':'jani.readiness/v1','ready':passed,'generated_cases':manifest['records'],
        'executed_cases':observed,'oracle_evidence_sufficient':coverage_ok,
        'passed_gates':sum(gates.values()),'mandatory_gates':len(MANDATORY),
        'failed_or_missing_gates':[k for k,v in gates.items() if not v],
        'disclaimer':'Synthetic data generation alone provides zero validation of Jani runtime safety.'}
def main():
    p=argparse.ArgumentParser();p.add_argument('--corpus',type=Path,required=True);p.add_argument('--receipts',type=Path)
    a=p.parse_args();report=evaluate(a.corpus,a.receipts);print(json.dumps(report,indent=2))
    raise SystemExit(0 if report['ready'] else 2)
if __name__=='__main__':main()
