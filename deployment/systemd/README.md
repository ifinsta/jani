# Supervised Sentinel Linux pilot (not a production installer)

These systemd units are templates, **not production-tested packages**. Start only after the native Rust compilation and the TLS/mTLS interop gates pass on the target environment.

1. Install verified binaries as /usr/local/bin/jani-probe and jani-gateway. Existing release artifacts do not contain these binaries yet.
2. Create dedicated least-privilege jani-probe and jani-gateway system identities. Never grant root access just to conceal missing log permissions.
3. Supply unique device credentials issued by an independent approved CA. Protect /etc/jani/pki private keys with ACLs; never copy a CA private key to the gateway/probe. Run tools/sentinel_registry.py enroll with the CA certificate, device certificate, tenant and device ID.
4. Configure /etc/jani/probe.env (JANI_GATEWAY, JANI_TENANT, JANI_DEVICE), /etc/jani/gateway.env (JANI_BIND) and /etc/jani/services.json with authorized absolute log paths. The units block access under users' home directories.
5. Install the respective .service into /etc/systemd/system, then run systemctl daemon-reload, systemctl enable --now jani-probe (or jani-gateway), and inspect systemctl status and journalctl for errors.
6. Test service restart, disconnected network, spool saturation, file rotation, certificate revocation, tenant isolation and outage recovery before customer pilots. Verify coverage_latest from the gateway store.

The gateway is single-node and has no HA failover, telemetry backup automation, certificate issuance, CA rollover, or signed update. Systemd protection directives can vary by distribution. A service unit is not proof of production readiness.
