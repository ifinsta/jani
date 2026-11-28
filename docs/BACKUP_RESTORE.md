---
id: JANI-OPS-008
title: "Backup and disaster recovery"
status: implemented-partial
owner: tools/state_backup.py
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - tools/state_backup.py
  - backend/core/src/recovery.rs
  - backend/server/src/main.rs
tests:
  - tools/test_state_backup.py
---

# Backup and disaster recovery

Use the operator tool to snapshot one local Jani `--state-dir` and restore it into a fresh inspection directory. The snapshot includes the journal, task and effect checkpoints, work queue, delivery files, approvals, budgets, nonce records, and other regular files and directories under that state root. A restore installs an inspection hold and a global kill switch before publishing any usable state path.

## Capture a cold snapshot

1. Stop ingress and all processes and operators that write to the selected state directory. Include UI sessions, queue watchers, dispatch workers and local administrative jobs. Keep them stopped through the snapshot. `--offline` records this prerequisite. Changes observed during the copy abort the operation. The tool cannot establish global writer exclusion.
2. Select a trusted local parent outside the source tree. Use an operator-only location and protect source, archive, and restore paths through filesystem access controls and storage encrypted with separately managed keys. Python 3.11.10 or later on the 3.11 branch, 3.12.4 or later on the 3.12 branch, and 3.13 and later create private Windows output directories. Keep backup copies under restricted custody. [Python mkdir](https://docs.python.org/3.12/library/os.html#os.mkdir)
3. Run the backup and retain `manifestSha256` in an independent trusted receipt store along with the capture time, source identity and verified audit cutoff. Retain any existing kill-switch policy and pending-effect information for reconciliation.

```text
python tools/state_backup.py backup --offline --state-dir ./runtime --out ./snapshot-001
```

Keep each source database and its WAL together from the same cold runtime. The tool verifies the four native SQLite stores on private copies. It checks the copied WAL header and frames before SQLite recovers committed rows into standalone databases. Related shared-memory indexes are rebuilt as necessary. An orphaned sidecar, an unexpected rollback journal, an ambiguous WAL tail, or a failed structural or foreign-key check blocks publication. SQLite can leave old frames after a valid WAL reset, so ambiguous tails require a controlled SQLite recovery and fresh backup. Other regular files, including SQLite-like files in a Git checkout, are copied as ordinary content. POSIX owner execute bits are retained. Links, reparse points, hardlinks, special files and cross-filesystem entries require separate handling. [SQLite WAL format](https://www.sqlite.org/fileformat2.html#walformat) and [SQLite integrity checks](https://www.sqlite.org/pragma.html#pragma_integrity_check)

Files outside `--state-dir`, including the independently held audit anchor, Git object and worktree registrations, gateway and probe stores at separate locations, and managed policy, identity and credentials, need their own backup and retention procedures. Protect credentials used today and retain revocation decisions outside the recoverable state. An older checkpoint cannot establish whether an upstream effect occurred after the capture.

## Verify and restore for inspection

1. Use the independently retained manifest digest. A matching SHA-256 authenticates the selected snapshot only to the extent that the pin is trusted. Test verification before attempting a restore.

   ```text
   python tools/state_backup.py verify --backup ./snapshot-001 --manifest-sha256 DIGEST
   python tools/state_backup.py restore --backup ./snapshot-001 --manifest-sha256 DIGEST --out ./restored-001
   ```

2. Use the returned `stateDir`, an isolated inspection worktree, a clean shell without inherited deployment `JANI_*` settings, and an explicit settings file containing `{"version":1}`. Use a Jani binary that recognizes `restore-hold.json`. Keep the restored path away from normal worker configuration. Never attach the production anchor as a writable sink during inspection.

   ```text
   jani audit verify --repo ./inspection --settings settings.json --state-dir ../restored-001/state
   jani recovery status --repo ./inspection --settings settings.json --state-dir ../restored-001/state
   jani agent kill-switch status --repo ./inspection --settings settings.json --state-dir ../restored-001/state
   ```

3. Compare `last_digest` with the independently recorded cutoff. For a deployment with complete anchor coverage, use the digest in the last trusted cutoff line of the append-only anchor. Chain validity alone does not prove the snapshot represents that cutoff. Missing or mismatched evidence blocks activation. Some runtime paths do not write to the optional anchor and require a separately observed cutoff.
4. Review the queue and durable effect rows in read-only mode. Compare earlier calls with the upstream source, revalidate live policies, revocations, budgets and approvals, and reconcile pending effects before accepting work. A relocated linked worktree may still point into the original repository. Git registration and branch state require explicit reconciliation. Keep both holds during inspection and let the authorized operator clear them only after this review. Removing the filesystem hold alone leaves the global kill switch in place. If the restore reports `priorKillSwitch`, reconcile that saved policy before replacing the temporary global stop. An older Jani binary cannot enforce the inspection hold.

An interrupted operation can leave a private container. A backup without `manifest.json` or a restore without `state` is incomplete. Select a new output path after recording and investigating the failure. The tool streams and flushes files, and syncs directory entries on POSIX. Windows directory-entry durability under sudden power loss and offline media retention require independent verification.

## Drill evidence

Run `python -m unittest tools.test_state_backup -v` to check WAL recovery, input stability, snapshot integrity and refusal of in-place recovery. Set `JANI_RECOVERY_DRILL_BIN` to a local Jani executable to also create a genuine anchored journal and queued task, recover an uncheckpointed committed row, and check that the restored CLI rejects live execution. The production gate requires signed evidence from the approved release campaign. [Release acceptance](RELEASE_ACCEPTANCE.md)

Release drills still need to measure recovery time and test disk-full conditions, loss of encryption keys, and restoration from older backups under an independent reviewer.
