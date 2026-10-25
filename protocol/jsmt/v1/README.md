# JSMT/1 — open-source communication protocol

**Jani Secure Machine Transport** is an experimental, versioned protocol for an independently managed client probe to communicate encrypted host telemetry to an authenticated central gateway. It is published under the [MIT license](../../../LICENSE). Although future versions may use proprietary code, this published version remains open under its existing license.

- [Normative protocol specification](SPEC.md)
- [Python interoperable reference](reference/python/jani_probe/schema.py)
- [TLS gateway](reference/python/jani_probe/gateway.py)
- [Durable client spool](reference/python/jani_probe/probe.py)
- [Service and host collectors](reference/python/jani_probe/collect.py)
- [E2E and adversarial tests](reference/python/tests/test_end_to_end.py)

## Execute the reference

On Linux with Python 3.11+ and OpenSSL CLI:

```bash
cd protocol/jsmt/v1/reference/python
python -m unittest discover -s tests -v
```

The reference test suite generates ephemeral credentials in temporary directories. **Do not commit any real private keys or deploy the example gateway to untrusted networks.** Its manual trust registry and missing key lifecycle do not satisfy the signed-enrollment and operations release requirements.

## Explicit boundary

The current application product remains the Rust `jani` agent. This reference uses Python and validates **only** the documented sample protocol. The repository does not currently ship a Rust `jani-probe` or `jani-gateway` executable and no such binaries may be declared as releases until built. Full OS log collection, host protections, tenant administration, AI investigation and managed enrollment are independent future milestones. Jani's current attention store is single-host and must not be used as the public ingestion broker.
