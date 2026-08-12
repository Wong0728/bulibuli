# Security policy

Please do not report security vulnerabilities in a public issue. Send a minimal reproduction and affected version privately to the repository maintainer, or use GitHub's private vulnerability reporting when enabled.

When reporting normal bugs, remove cookies, tokens, pairing codes, database files, download metadata and absolute local paths. The service defaults to loopback, uses local-only IPC for `ctl`, and expects TLS verification to remain enabled.

Deployment hardening, LAN/proxy warnings and systemd guidance are documented in [`deploy/SECURITY.md`](deploy/SECURITY.md).
