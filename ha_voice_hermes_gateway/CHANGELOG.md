# Changelog

## 0.1.0

- Add the experimental, same-repository Home Assistant App for a locally hosted
  realtime gateway.
- Run the optimized Rust/WASM Worker under pinned `workerd` with local-disk
  Durable Object persistence.
- Add direct TLS using Home Assistant's read-only `/ssl` mount, certificate
  change detection, publicly trusted outbound TLS, and explicit private-network
  upstream consent.
- Add masked configuration fields for Hermes, ElevenLabs, Cloudflare Access,
  and unique per-device credentials and memory scopes.
- Use cold backups for consistent local conversation metadata.
- Fail closed on invalid option changes, restrict header-bound credentials to
  visible ASCII, and isolate provider secrets from process arguments and
  inherited Supervisor environment.
- Keep a validated certificate only during a bounded, health-degraded renewal
  grace period when options are unchanged; stop at expiry or persistent error.
- Add native multi-architecture smoke, SBOM, vulnerability/secret, artifact-PII,
  AppArmor, metadata, and immutable signed-publication gates.
