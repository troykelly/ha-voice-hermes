# Voice PE feature compatibility

This project deliberately replaces the stock Home Assistant voice-assistant
pipeline; it does not claim byte-for-byte feature parity with the current
upstream Voice PE firmware. The table below is the release contract.

| Capability | Status | Notes |
| --- | --- | --- |
| XMOS far-field microphone processing and echo-reference path | Retained | Uses the upstream `voice_kit` component and pinned XMOS firmware. Acoustic performance still requires physical testing. |
| On-device wake word | Retained | Wake detection remains local and starts a Hermes turn. |
| Physical mute, center button, volume dial and LED ring | Retained | Includes conversation reset, cancellation, volume and factory-reset gestures. |
| Realtime Hermes capture and playback | Replaced | Uses the authenticated v2 WebSocket protocol, ElevenLabs STT/TTS and Hermes Responses API. |
| ESPHome Improv BLE/Serial, Native API, OTA and safe mode | Retained | Factory adoption follows the official Voice PE resource lifecycle. |
| Home Assistant music and announcements | Retained, basic | Separate bounded HTTP-FLAC media and announcement inputs feed the shared mixer. They do not carry Hermes transcripts or credentials. |
| Home Assistant voice-assistant pipeline | Removed | Hermes owns conversational turns; Home Assistant is optional for management and media. |
| Sendspin/group playback | Not currently included | Group synchronization and the upstream Sendspin media player are outside the present media contract. |
| Home Assistant timers and timer-finished UI/audio | Not currently included | A Hermes tool may manage timers independently, but the stock Voice PE timer experience is not reproduced. |
| Stock local UI sounds | Not currently included | Wake, mute, jack, button and factory-reset sound assets are not shipped; the LED state machine remains. |
| Headphone-jack UX | Not currently included | Do not assume stock jack detection, routing sounds or event entities. |
| Managed firmware update entity | Not currently included | Updates use normal ESPHome OTA/USB recovery and immutable repository releases. |
| Future proactive speech | Designed, not implemented | See `proactive-delivery.md`; announcements and contextual invitations have different continuity semantics. |
| Future wake-word-to-agent routing | Designed, not implemented | See `multi-agent-wake-routing.md`; each route has an isolated conversation lane and fail-closed policy. |

Home Assistant audio is ducked while Hermes captures or speaks, but ducking is
not acoustic isolation. Release testing must measure AEC leakage, self-wake,
recognition accuracy and intelligibility while music or an announcement is
active.
