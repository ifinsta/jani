"""Synthetic-only record review benchmark; not Jani inference or a banking connector.

Run against a fresh local SQLite DB. One committed result per source record.
The test clock is explicit for lease/retry injection; production needs trusted
time, durable shared storage, authenticated workers and source authorization.
"""
import argparse
import hashlib
import json
import sqlite3
import time
from pathlib import Path

BATCH = 1000


def setup(db, count):
    db.executescript("""
        CREATE TABLE customer(id INTEGER PRIMARY KEY, customer_ref INTEGER NOT NULL,
                              kyc INTEGER NOT NULL, balance_cents INTEGER NOT NULL);
        CREATE TABLE queue(id INTEGER PRIMARY KEY, first_id INTEGER NOT NULL,
            last_id INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'ready',
            epoch INTEGER NOT NULL DEFAULT 0, owner TEXT, expires INTEGER);
        CREATE TABLE review(id INTEGER PRIMARY KEY REFERENCES customer(id),
                            mask INTEGER NOT NULL, evidence TEXT NOT NULL);
    """)
    def rows():
        for i in range(1, count + 1):
            yield i, i - (i % 113 == 0), int(i % 97 != 0), -100 if i % 101 == 0 else 100
    with db:
        db.executemany("INSERT INTO customer VALUES (?,?,?,?)", rows())
        db.execute("CREATE INDEX customer_ref_index ON customer(customer_ref)")
        db.executemany("INSERT INTO queue(id,first_id,last_id) VALUES(?,?,?)",
                       ((p // BATCH + 1, p + 1, min(p + BATCH, count)) for p in range(0, count, BATCH)))


def claim(db, owner, now):
    db.execute("BEGIN IMMEDIATE")
    try:
        row = db.execute("SELECT id,first_id,last_id,epoch FROM queue "
                         "WHERE state='ready' OR (state='leased' AND expires<=?) ORDER BY id LIMIT 1",
                         (now,)).fetchone()
        if row is None:
            db.commit()
            return None
        job, first, last, epoch = row
        db.execute("UPDATE queue SET state='leased',epoch=?,owner=?,expires=? WHERE id=?",
                   (epoch + 1, owner, now + 30, job))
        db.commit()
        return job, first, last, epoch + 1
    except BaseException:
        db.rollback()
        raise


def inspect(db, job):
    _, first, last, _ = job
    # Scan actual source fields, not the generator's injected-fault formula.
    rows = db.execute("""SELECT c.id,c.customer_ref,c.kyc,c.balance_cents,
          EXISTS(SELECT 1 FROM customer d WHERE d.customer_ref=c.customer_ref AND d.id<>c.id)
          FROM customer c WHERE c.id BETWEEN ? AND ? ORDER BY c.id""", (first, last))
    results = []
    for ident, ref, kyc, balance, duplicate in rows:
        mask = int(kyc == 0) | (int(balance < 0) << 1) | (int(duplicate) << 2)
        evidence = hashlib.sha256(json.dumps([ident, ref, kyc, balance, duplicate, mask],
                                            separators=(",", ":")).encode()).hexdigest()
        results.append((ident, mask, evidence))
    return results


def commit(db, job, owner, now, rows, inject_failure=False):
    ident, first, last, epoch = job
    if [r[0] for r in rows] != list(range(first, last + 1)):
        raise ValueError("partition coverage mismatch")
    db.execute("BEGIN IMMEDIATE")
    try:
        current = db.execute("SELECT state,epoch,owner,expires FROM queue WHERE id=?",
                             (ident,)).fetchone()
        if current is None or current[:3] != ("leased", epoch, owner) or current[3] <= now:
            raise PermissionError("stale assignment")
        db.executemany("INSERT INTO review VALUES (?,?,?)", rows)
        if inject_failure:
            raise RuntimeError("injected interruption before commit")
        db.execute("UPDATE queue SET state='done',owner=NULL,expires=NULL WHERE id=?",
                   (ident,))
        db.commit()
    except BaseException:
        db.rollback()
        raise

def source_digest(db):
    h = hashlib.sha256()
    for row in db.execute("SELECT id,customer_ref,kyc,balance_cents FROM customer ORDER BY id"):
        h.update((",".join(map(str, row)) + "\n").encode())
    return h.hexdigest()


def benchmark(path, count):
    if not 1 <= count <= 1_000_000 or path.exists():
        raise ValueError("fresh database and count 1..1000000 required")
    started = time.monotonic()
    db = sqlite3.connect(path)
    try:
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=FULL")
        setup(db, count)
        original = source_digest(db)
        old = claim(db, "worker-a", 1)
        rows = inspect(db, old)
        checks = {}
        try:
            commit(db, old, "worker-a", 2, rows, inject_failure=True)
        except RuntimeError:
            checks["rollback_no_partial_results"] = db.execute("SELECT COUNT(*) FROM review").fetchone()[0] == 0
        replacement = claim(db, "worker-b", 31)
        checks["expired_lease_reclaimed"] = replacement[:3] == old[:3] and replacement[3] == old[3] + 1
        try:
            commit(db, old, "worker-a", 32, rows)
        except PermissionError:
            checks["stale_worker_rejected"] = True
        commit(db, replacement, "worker-b", 32, inspect(db, replacement))
        try:
            commit(db, replacement, "worker-b", 32, rows)
        except PermissionError:
            checks["duplicate_commit_rejected"] = True
        while (job := claim(db, "worker-b", 32)) is not None:
            commit(db, job, "worker-b", 32, inspect(db, job))
        total, flagged, missing, negative, duplicate = db.execute(
            "SELECT COUNT(*),SUM(mask<>0),SUM((mask&1)<>0),SUM((mask&2)<>0),SUM((mask&4)<>0) FROM review"
        ).fetchone()
        wrong = db.execute("""SELECT COUNT(*) FROM review WHERE mask !=
            ((id%97=0) | ((id%101=0)<<1) |
            (((id%113=0) OR (id%113=112 AND id<?))<<2))""", (count,)).fetchone()[0]
        checks["every_record_reviewed_once"] = total == count and db.execute(
            "SELECT COUNT(*) FROM customer c LEFT JOIN review r ON c.id=r.id WHERE r.id IS NULL"
        ).fetchone()[0] == 0
        checks["exact_expected_flags"] = wrong == 0 and (missing,negative,duplicate) == (count//97,count//101,2*(count//113))
        checks["no_source_mutation"] = original == source_digest(db)
        checks["all_partitions_done"] = db.execute("SELECT COUNT(*) FROM queue WHERE state!='done'").fetchone()[0] == 0
        checks["all_evidence_present"] = db.execute("SELECT COUNT(*) FROM review WHERE length(evidence)!=64").fetchone()[0] == 0
        checks["database_integrity"] = db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
        expected_names = {"rollback_no_partial_results","expired_lease_reclaimed","stale_worker_rejected","duplicate_commit_rejected","every_record_reviewed_once","exact_expected_flags","no_source_mutation","all_partitions_done","all_evidence_present","database_integrity"}
        return {"ok": set(checks)==expected_names and all(checks.values()), "synthetic_only":True,
                "model_calls":0,"source_records":count,"reviewed":total,"flagged":flagged,"clear":total-flagged,
                "flag_counts":{"missing_kyc":missing,"negative_balance_test_rule":negative,"duplicate_reference":duplicate},
                "wrong_masks":wrong,"partitions":(count+BATCH-1)//BATCH,"source_sha256":original,
                "checks":checks,"seconds":round(time.monotonic()-started,3)}
    finally:
        db.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--count", type=int, default=1000)
    args = parser.parse_args()
    result = benchmark(args.db, args.count)
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result["ok"] else 1)
