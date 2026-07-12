# Security policy

This project handles microphone audio, conversation text, provider credentials,
and a tool-capable Hermes agent. Please do not open a public issue containing a
credential, transcript, recording, private hostname, device identifier, or
exploit payload.

Report vulnerabilities through GitHub's private vulnerability reporting for
this repository. Include the affected version, impact, and a minimal redacted
reproduction. Rotate any credential that may have been disclosed before sharing
diagnostic material.

Only immutable tagged releases that have passed the acceptance gates in
[`docs/testing.md`](docs/testing.md) are supported for device installation.
The `main` branch is development source and may intentionally remain ahead of a
published firmware release.
