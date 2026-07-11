# Canonical realtime v3 roadmap

Realtime v3 is one future protocol, not separate proactive-delivery and
multi-agent variants. It is not implemented and none of the controls below are
accepted by a v2 gateway or device.

## Negotiated capabilities

The v3 `hello` advertises independently versioned capabilities:

- `agent_routes`: route-manifest hash, compiled route IDs and locally enabled
  route IDs;
- `proactive_delivery`: announcement support, contextual-invitation support,
  local consent policy and maximum supported offer version;
- the unchanged PCM format, frame size and consumption-credit contract.

`ready` returns the Worker-authorized intersection plus a registry/policy
revision. A device without an accepted route cannot begin capture. A device
without accepted proactive capability never receives unsolicited output. There
is no fallback from an unknown route to a default agent and no downgrade of a
contextual invitation into an announcement.

## One control namespace

The same v3 schema contains:

- route-scoped `turn.start`, `turn.ready` and `conversation.reset`;
- `delivery.offer`, `delivery.accept`/`reject`, server-owned output turns and
  `delivery.drained`;
- group `delivery.claim`/`claimed`/`withdrawn`;
- contextual `delivery.attached`, `delivery.reply.start` and
  `delivery.detach`/`detached`;
- the existing binary PCM header and cumulative input/output acknowledgements.

Every control carries the relevant route/lane or delivery/session identifier.
The gateway latches those identifiers before audio and rejects any later frame
whose turn, lane, lease epoch or claim token does not match.

## Combined precedence

Physical mute always wins. An active device capture outranks ordinary delivery;
ordinary offers queue or expire. A contextual attachment owns at most one
reply lane. Selecting a distinct wake route first durably detaches that lease,
then starts the personal destination lane. A group invitation admits exactly
one reply claimant through a server-side compare-and-swap. Reset never guesses:
it targets an explicit personal route or an explicitly authorized group
conversation.

## Version skew and rollout

- v2 and v3 use different WebSocket subprotocol names.
- A v2 device continues to receive only v2 controls and can coexist with v3
  devices in separate per-device sessions.
- A v3 device rejects a server that omits required capability results or returns
  an unknown registry revision.
- The Worker keeps route and proactive policy authoritative; client manifests
  are drift signals, not hardware attestation.
- Rollout begins with announcements, then single-device contextual invitations,
  then multi-agent routing, and finally group claims. Each stage remains
  disabled until its fault, consent, lease and downgrade tests pass.

The detailed semantics remain in [`proactive-delivery.md`](proactive-delivery.md)
and [`multi-agent-wake-routing.md`](multi-agent-wake-routing.md). Any future
implementation must first turn this roadmap into one versioned JSON schema and
one shared device/gateway conformance suite.
