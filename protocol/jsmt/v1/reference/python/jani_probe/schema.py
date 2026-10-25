"""Open-source JSMT/1 Python wire reference. Not an official OTel/OTLP encoder."""
from __future__ import annotations
import hashlib
import json
import math
import re
from datetime import datetime, timezone

VERSION = "jani-jsmt/1"
MAX_PAYLOAD_BYTES = 256 * 1024
MAX_LOGS = 200
MAX_METRICS = 256
MAX_COVERAGE = 256
METRICS = {
    "system.uptime": "s",
    "system.cpu.time": "s",
    "system.memory.usage": "By",
    "system.process.count": "{process}",
    "jani.filesystem.used": "By",
    "jani.service.up": "1",
    "jani.service.log.covered": "1",
}
SECRET = re.compile(r"(?i)(?:authorization\s*[:=]\s*bearer\s+|(?:password|api[_-]?key|token|secret)\s*[:=]\s*)([^\s,;]+)")

def redact(message: str) -> str:
    return SECRET.sub(lambda m: m.group(0)[:m.start(1)-m.start(0)] + "[REDACTED]", message)

def canonical(obj: object) -> bytes:
    return json.dumps(obj, separators=(",", ":"), sort_keys=True, ensure_ascii=False, allow_nan=False).encode("utf-8")

def digest(obj: object) -> str:
    return hashlib.sha256(canonical(obj)).hexdigest()

def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

def normalize_metric(name: str, value: float, attributes: dict | None = None) -> dict:
    if name not in METRICS or isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError("invalid metric name/value")
    attrs = attributes or {}
    if not isinstance(attrs, dict) or len(attrs) > 8 or any(not isinstance(k, str) or not isinstance(v, str) or len(v) > 100 for k, v in attrs.items()):
        raise ValueError("unbounded metric attributes")
    return {"name": name, "unit": METRICS[name], "value": float(value), "attributes": attrs}

def normalize_log(service: str, body: str, source: str, severity: str = "INFO") -> dict:
    if not service or len(service) > 100 or not source or len(source) > 128:
        raise ValueError("invalid log source")
    if severity not in {"TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"}:
        severity = "INFO"
    return {"time_unix_nano": int(datetime.now(timezone.utc).timestamp() * 1_000_000_000),
            "severity_text": severity, "body": redact(str(body))[:4096],
            "resource": {"service.name": service}, "attributes": {"jani.log.source": source}}

def validate(envelope: dict) -> None:
    if not isinstance(envelope, dict) or set(envelope) != {"version", "tenant_id", "device_id", "sequence", "observed_at", "metrics", "logs", "coverage"}:
        raise ValueError("unexpected envelope fields")
    if envelope["version"] != VERSION:
        raise ValueError("unsupported protocol version")
    for field in ("tenant_id", "device_id"):
        val = envelope[field]
        if not isinstance(val, str) or not re.fullmatch(r"[a-zA-Z0-9_.-]{1,96}", val):
            raise ValueError("invalid identity")
    if type(envelope["sequence"]) is not int or not (1 <= envelope["sequence"] < 2**63):
        raise ValueError("invalid sequence")
    if not isinstance(envelope["observed_at"], str) or len(envelope["observed_at"]) > 64:
        raise ValueError("invalid time")
    for k, cap in (("metrics", MAX_METRICS), ("logs", MAX_LOGS), ("coverage", MAX_COVERAGE)):
        if not isinstance(envelope[k], list) or len(envelope[k]) > cap:
            raise ValueError("invalid or oversized collection")
    for metric in envelope["metrics"]:
        if not isinstance(metric, dict) or set(metric) != {"name", "unit", "value", "attributes"}:
            raise ValueError("invalid metric record")
        normalize_metric(metric["name"], metric["value"], metric["attributes"])
        if metric["unit"] != METRICS[metric["name"]]:
            raise ValueError("invalid metric unit")
    for log in envelope["logs"]:
        if not isinstance(log, dict) or set(log) != {"time_unix_nano", "severity_text", "body", "resource", "attributes"}:
            raise ValueError("invalid log record")
        if type(log["time_unix_nano"]) is not int or len(log["body"]) > 4096 or log["body"] != redact(log["body"]):
            raise ValueError("unsafe log record")
        if log["severity_text"] not in {"TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"}:
            raise ValueError("invalid log severity")
        if not isinstance(log["resource"], dict) or not re.fullmatch(r"[a-zA-Z0-9_.@-]{1,100}", log["resource"].get("service.name", "")):
            raise ValueError("invalid log service")
        if not isinstance(log["attributes"], dict) or len(log["attributes"]) > 8:
            raise ValueError("invalid log attrs")
    for item in envelope["coverage"]:
        if not isinstance(item, dict) or set(item) != {"service", "source", "status", "reason"}:
            raise ValueError("invalid coverage record")
        if item["status"] not in {"covered", "partial", "missing", "permission_denied", "unsupported"}:
            raise ValueError("invalid coverage status")
        if any(not isinstance(item[k], str) or len(item[k]) > 160 for k in ("service", "source", "reason")):
            raise ValueError("invalid coverage details")
    if len(canonical(envelope)) > MAX_PAYLOAD_BYTES:
        raise ValueError("oversized envelope")
