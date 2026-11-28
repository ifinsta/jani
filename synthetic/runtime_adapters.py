"""Local runtime adapters for selected audit, broker, and budget fault hooks."""

from dataclasses import dataclass
import hashlib
import json

from .evidence_io import decode_object
from .runtime_harness import FIXTURE_TEXT, bounded_read, encoded, trace_bytes, verify_journal


AUDIT_FAULTS = {
    "audit.corrupt_hash_chain", "audit.deleted_event", "audit.inserted_event",
    "audit.reordered_events", "audit.incomplete_jsonl",
}
MODEL_FAULTS = {"tool_broker.unknown_tool", "tool_broker.write_without_grant", "budget.tool_limit_crossed"}
SUPPORTED_HOOKS = frozenset(AUDIT_FAULTS | MODEL_FAULTS)
FIXTURE_ANSWER = "Local synthetic fixture processed."


@dataclass(frozen=True)
class Plan:
    hook: str
    clean: bool


def choose(row):
    hook = row["replay"]["fault_hook"]
    if hook not in SUPPORTED_HOOKS:
        return None, "NO_RUNTIME_ADAPTER"
    if row["profile"] != "cold_start":
        return None, "PROFILE_NOT_IMPLEMENTED"
    if row["secondary"] is not None or row["tertiary"] is not None:
        return None, "COMPOUND_FAULT_NOT_IMPLEMENTED"
    if row["context"]["tenant"] != row["context"]["source_tenant"]:
        return None, "CROSS_TENANT_CONTEXT_NOT_IMPLEMENTED"
    if row["mutation"] not in {"baseline", "clean_valid_context"}:
        return None, "MUTATION_NOT_IMPLEMENTED"
    clean = row["mutation"] == "clean_valid_context"
    decision = "PERMIT" if clean else "FAIL_CLOSED" if hook in AUDIT_FAULTS else "DENY"
    if row["oracle"]["decision"] != decision:
        return None, "FIXTURE_ORACLE_CONFLICT"
    return Plan(hook, clean), None


def finish_reply():
    return {"choices": [{"message": {"role": "assistant", "content": FIXTURE_ANSWER}}]}


def calls_reply(*calls):
    return {"choices": [{"message": {"role": "assistant", "content": None, "tool_calls": [
        {"id": f"call-{index}", "type": "function", "function": {"name": tool, "arguments": encoded(args).decode("ascii")}}
        for index, (tool, args) in enumerate(calls, 1)
    ]}}]}


def observations(requests):
    if len(requests) != 2:
        raise ValueError("expected one model round to request tools and one to observe their results")
    tools = [msg for msg in requests[1].get("messages", []) if msg.get("role") == "tool"]
    if [msg.get("tool_call_id") for msg in tools] != [f"call-{index}" for index in range(1, len(tools) + 1)]:
        raise ValueError("tool result correlation differs from the fixture calls")
    return [decode_object(message["content"].encode("utf-8")) for message in tools]


def check_tool_events(events, results, action, resource, denied_reason=None):
    invocations = [result.get("invocationId") for result in results]
    if (any(not isinstance(invocation, str) or not invocation for invocation in invocations)
            or len(set(invocations)) != len(invocations)):
        raise ValueError("tool results lack distinct native invocation identifiers")
    actual = [(e["invocation_id"], e["action"], e["resource_digest"], e["decision"], e["reason_code"])
              for e in events if e["action"] != "agent.run"]
    digest = hashlib.sha256(resource.encode()).hexdigest()
    expected = []
    for invocation, result in zip(invocations, results, strict=True):
        decisions = [("permit", None), ("executed", "ok")] if result["status"] == "ok" else [("deny", denied_reason)]
        expected.extend((invocation, action, digest, decision, reason) for decision, reason in decisions)
    if len(expected) != len(actual) or any(observed[:4] != wanted[:4]
            or (wanted[4] is not None and observed[4] != wanted[4])
            for observed, wanted in zip(actual, expected, strict=True)):
        raise ValueError("native journal does not match the exact tool invocation, action, resource, or decision")


def check_run(fixture, result):
    if result.exit_code != 0 or result.stdout.strip() != FIXTURE_ANSWER.encode():
        raise ValueError("Jani did not finish the fixture run with the expected loopback answer")
    raw = bounded_read(fixture.journal)
    events = verify_journal(raw)
    if any((event.get("run_id"), event.get("tenant_id"), event.get("task_id"), event.get("actor_id"))
           != (fixture.run_id, fixture.tenant, fixture.task, fixture.principal) for event in events):
        raise ValueError("uncorrelated audit event in the fixture journal")
    runs = [(i, event["decision"]) for i, event in enumerate(events) if event["action"] == "agent.run"]
    if runs != [(0, "started"), (len(events) - 1, "completed")]:
        raise ValueError("missing or contradictory fixture run outcome")
    expected_anchor = "".join(f"{e['timestamp']} {e['event_id']} {e['event_digest']}\n" for e in events).encode("ascii")
    if bounded_read(fixture.anchor) != expected_anchor:
        raise ValueError("audit anchor diverges from the native events")
    return raw, events


def check_authentic_audit(fixture, raw, events, anchor):
    result = fixture.invoke("audit", "verify")
    report = decode_object(result.stdout)
    if (result.exit_code != 0 or report.get("ok") is not True or report.get("events") != len(events)
            or report.get("last_digest") != events[-1]["event_digest"] or bounded_read(fixture.journal) != raw
            or bounded_read(fixture.anchor) != anchor):
        raise ValueError("native verifier did not corroborate the independently verified journal")


def damaged_journal(hook, raw):
    lines = raw.splitlines(keepends=True)
    if len(lines) < 4:
        raise ValueError("fault fixture requires multiple anchored events")
    if hook == "audit.corrupt_hash_chain":
        event = json.loads(lines[1])
        event["event_digest"] = ("0" if event["event_digest"][0] != "0" else "1") + event["event_digest"][1:]
        lines[1] = json.dumps(event, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
    elif hook == "audit.deleted_event":
        del lines[1]
    elif hook == "audit.inserted_event":
        lines.insert(1, lines[2])
    elif hook == "audit.reordered_events":
        lines[1], lines[2] = lines[2], lines[1]
    elif hook == "audit.incomplete_jsonl":
        lines[-1] = lines[-1][:-9]
    else:
        raise ValueError("unknown audit fault")
    return b"".join(lines)


def run_audit(fixture, plan):
    result, transcript = fixture.run([calls_reply(("read", {"path": fixture.target.name})), finish_reply()])
    raw, events = check_run(fixture, result)
    reads = observations(transcript)
    if (len(reads) != 1 or reads[0].get("status") != "ok"
            or reads[0].get("data", {}).get("contents") != FIXTURE_TEXT.decode()):
        raise ValueError("audit setup did not execute its positive read control")
    check_tool_events(events, reads, "repo.read", f"repo:{fixture.target.name}")
    anchor = bounded_read(fixture.anchor)
    check_authentic_audit(fixture, raw, events, anchor)
    evidence = {"before_journal": trace_bytes(raw), "audit_chain_before": "valid"}
    if plan.clean:
        return "PERMIT", evidence
    corrupted = damaged_journal(plan.hook, raw)
    fixture.journal.write_bytes(corrupted)
    try:
        verify_journal(corrupted)
    except ValueError:
        pass
    else:
        raise ValueError("the injected audit fault did not invalidate the original audit chain")
    result = fixture.invoke("audit", "verify")
    if plan.hook == "audit.incomplete_jsonl":
        final_line = len(raw.splitlines())
        refusal = (f"journal line {final_line} is invalid: incomplete line".encode() in result.stderr
                   and not result.stdout)
    else:
        report = decode_object(result.stdout)
        reason = ("event digest mismatch" if plan.hook == "audit.corrupt_hash_chain"
                  else "previous_event_digest does not match the preceding event")
        refusal = (report.get("ok") is False and report.get("failure_line") == 2 and report.get("reason") == reason
                   and b"audit journal verification failed" in result.stderr)
    if (result.exit_code != 1 or not refusal or bounded_read(fixture.journal) != corrupted
            or bounded_read(fixture.anchor) != anchor):
        raise ValueError("native audit did not refuse the actual fault without modifying it")
    startup, requests = fixture.run([])
    expected_error = b"incomplete line" if plan.hook == "audit.incomplete_jsonl" else b"audit journal integrity failed"
    if (startup.exit_code != 1 or expected_error not in startup.stderr or startup.stdout or requests
            or bounded_read(fixture.journal) != corrupted or bounded_read(fixture.anchor) != anchor):
        raise ValueError("Jani startup did not preserve the rejected journal and stop before model use")
    return "FAIL_CLOSED", {**evidence, "audit_chain_after": "invalid", "refused_by_native_audit": True,
                           "refused_at_runtime_startup": True, "provider_requests_after_fault": len(requests),
                           "provider_connections_after_fault": fixture.provider_traces[-1].connections}


def run_model(fixture, plan, row):
    hook = plan.hook
    args = []
    write = hook == "tool_broker.write_without_grant"
    budget = hook == "budget.tool_limit_crossed"
    content = f"local result for {row['context']['resource']}\n"
    read = ("read", {"path": fixture.target.name})
    expected_file = FIXTURE_TEXT
    contract_bytes = None
    if write:
        calls = [("write", {"path": fixture.target.name, "content": content})]
        if plan.clean:
            args.extend(("--allow-write", "--write-path", fixture.target.name))
            expected_file = content.encode()
        expected_statuses = ["ok" if plan.clean else "denied"]
        action = "repo.write"
        denied_reason = None if plan.clean else "operator_write_denied"
    elif budget:
        limit = 2 if plan.clean else 1
        contract_bytes = encoded({"apiVersion": "jani.dev/v1alpha1", "kind": "WorkContract",
            "metadata": {"taskId": fixture.task, "principal": fixture.principal, "tenant": fixture.tenant},
            "spec": {"purpose": "SOFTWARE_DEV", "capabilities": ["repo.read"],
                     "budget": {"maxToolCalls": limit}}})
        contract = fixture.root / "contract.json"
        contract.write_bytes(contract_bytes)
        args.extend(("--contract", str(contract)))
        calls = [read, read]
        expected_statuses = ["ok", "ok" if plan.clean else "denied"]
        action = "repo.read"
        denied_reason = None if plan.clean else "budget_exhausted"
    else:
        calls = [read] if plan.clean else [("fixture_unknown_tool", {})]
        expected_statuses = ["ok" if plan.clean else "denied"]
        action = "repo.read" if plan.clean else "tool.unknown"
        denied_reason = None if plan.clean else "unknown_tool"

    result, transcript = fixture.run([calls_reply(*calls), finish_reply()], *args)
    raw, events = check_run(fixture, result)
    received = observations(transcript)
    if [value.get("status") for value in received] != expected_statuses:
        raise ValueError("provider observed a different tool decision than the fault requires")
    if any(value.get("data") is not None for value in received if value["status"] == "denied"):
        raise ValueError("a denied tool returned data")
    if action == "repo.read" and any(value.get("data", {}).get("contents") != FIXTURE_TEXT.decode()
                                     for value in received if value["status"] == "ok"):
        raise ValueError("the permitted read did not return the fixture bytes")
    resource = "tool:fixture_unknown_tool" if action == "tool.unknown" else f"repo:{fixture.target.name}"
    check_tool_events(events, received, action, resource, denied_reason)
    if budget and not plan.clean and received[1].get("error") != "tool-call budget exhausted (1)":
        raise ValueError("the second read failed for a reason other than the tool-call limit")
    check_authentic_audit(fixture, raw, events, bounded_read(fixture.anchor))
    fixture.assert_scope(expected_file)
    facts = {"tool_statuses": [value["status"] for value in received], "audit_verified": True,
             "journal_tool_effects": expected_statuses.count("ok"), "target_after_sha256": hashlib.sha256(bounded_read(fixture.target)).hexdigest()}
    if budget:
        paths = list((fixture.state / "budgets").glob("*.json"))
        if len(paths) != 1:
            raise ValueError("expected exactly one isolated fixture budget ledger")
        ledger = bounded_read(paths[0])
        observed = decode_object(ledger)
        if (observed.get("toolCalls"), observed.get("maxToolCalls"), observed.get("taskId"), observed.get("tenantId")) != (
                limit, limit, fixture.task, fixture.tenant):
            raise ValueError("native persisted budget does not corroborate tool admission")
        facts.update(budget=trace_bytes(ledger), contract=trace_bytes(contract_bytes))
    return ("PERMIT" if plan.clean else "DENY"), facts


def execute(fixture, plan, row):
    if plan.hook in AUDIT_FAULTS:
        result = run_audit(fixture, plan)
        fixture.assert_scope(FIXTURE_TEXT)
    else:
        result = run_model(fixture, plan, row)
    fixture.check_canary()
    return result
