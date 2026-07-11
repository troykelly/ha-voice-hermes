# ESPHome factory provisioning, adoption and management

## Status and conclusion

This is a **current release requirement**. The Hermes firmware must remain a normal ESPHome project for provisioning, Home Assistant discovery, Device Builder adoption, encrypted Native API ownership, logs, native OTA, safe mode and USB recovery.

It is technically viable on Voice PE. The implementation follows the official Voice PE factory pattern: physically authorized Improv over BLE, Improv Serial, project and Dashboard Import metadata, Native API/mDNS discovery, and BLE shutdown before realtime audio. The ESP32-S3 hardware does not prevent this.

The source and generated binaries are build-validated, but “100% compatible” must remain a release acceptance claim rather than a compile-time claim. It requires the physical proxy/serial/adoption/OTA/fault tests at the end of this document on erased Voice PEs; those tests have not been run in this checkout.

One external release prerequisite remains: `dashboard_import.package_import_url` and the adopted package's custom component source must resolve from the **public repository at an immutable release tag**. The repository is `troykelly/ha-voice-hermes`, but the configured `v0.3.0` release tag must not be published until the release-blocking tests pass. Do not distribute a factory binary until that tag exists and has been tested from a clean ESPHome installation.

## “Through a Bluetooth proxy” precisely

The device advertises the standard Improv BLE service. This is not a custom proxy protocol and Voice PE is not itself configured as a `bluetooth_proxy`.

Home Assistant can reach that Improv service through either:

- a local Bluetooth adapter;
- the Home Assistant Companion app's phone Bluetooth; or
- an existing ESPHome Bluetooth proxy with **active GATT connections** and a free connection slot.

A passive advertisement-only proxy can discover the device but cannot perform the GATT provisioning transaction. Home Assistant's [Improv BLE integration](https://www.home-assistant.io/integrations/improv_ble/) handles the device connection, Wi-Fi credentials and flow hand-off; ESPHome's [Bluetooth proxy documentation](https://esphome.io/components/bluetooth_proxy/) describes the active-connection requirement. The official [Voice PE factory YAML](https://github.com/esphome/home-assistant-voice-pe/blob/0579e7b9d8504264719c593474c85447253c9dc1/home-assistant-voice.factory.yaml#L1-L57) uses the same physical center-button authorization and BLE lifecycle.

Do not add `bluetooth_proxy` or `esp32_ble_tracker` to this Voice PE configuration. They serve a different purpose and would keep BLE scanning active beside a memory- and radio-intensive realtime audio workload.

## Four separate onboarding stages

```text
Improv BLE/Serial
          │ puts the factory image on 2.4 GHz Wi-Fi
          ▼
ESPHome Native API + mDNS
          │ Home Assistant discovers/owns the node
          ▼
Dashboard Import / Device Builder Adopt
          │ creates adopter-owned YAML + unique API key, then OTA installs it
          ▼
Post-adoption gateway enrollment
          │ adopter adds Worker URL + device token + OTA password, then installs again
          ▼
Hermes realtime voice enabled
```

These stages must not be conflated:

1. Improv BLE or Improv Serial configures Wi-Fi only.
2. Native API/mDNS makes the node discoverable by Home Assistant and chains the Improv flow into ESPHome setup.
3. Dashboard Import creates an editable configuration in ESPHome Device Builder. ESPHome requires `api`, `esphome.project`, and a public `dashboard_import` package URL for this operation; see [ESPHome's sharing contract](https://esphome.io/guides/creators/).
4. Standard ESPHome configuration/OTA adds the unit's Worker credential and closes the temporary passwordless-OTA handoff. Improv does not provision arbitrary Hermes settings.

Hermes and ElevenLabs API keys, Cloudflare Access credentials, model choices and voice IDs remain Worker-side. The device receives only a credential-free `wss://` Worker URL and a unique device bearer.

## Firmware artifacts

| File | Purpose | Secrets allowed |
| --- | --- | --- |
| [`hermes-voice-pe.factory.yaml`](../firmware/hermes-voice-pe.factory.yaml) | Build the universal first-flash/factory image from a repository checkout. | None. |
| [`hermes-voice-pe.yaml`](../firmware/hermes-voice-pe.yaml) | Public Dashboard Import package; fetches the custom component from the published repository. | None. |
| [`packages/hermes-voice-pe-base.yaml`](../firmware/packages/hermes-voice-pe-base.yaml) | Shared Voice PE hardware, audio, provisioning, API, OTA, HA media and Hermes configuration. | None. |
| [`hermes-voice-pe.local.yaml`](../firmware/hermes-voice-pe.local.yaml) | Clone-local/manual operator wrapper and reference for the adopter-owned YAML. | ESPHome/Wi-Fi and per-device Worker credential only. |

The public package deliberately compiles with `gateway_url` and `auth_token` both empty. In that state:

- no WebSocket task, reconnect loop, microphone capture or wake inference starts;
- API, OTA, logs, Improv, HA media and physical factory reset remain available;
- **Hermes Configured** reports false and the idle ring is amber;
- **Hermes Device ID** exposes the non-secret, hardware-stable `voice-pe-<wifi-mac>` identifier needed for the Worker's device-token map.

Exactly one gateway field is a configuration error. Both valid fields enable Hermes. The device token is marked sensitive in the ESPHome schema and must be supplied with `!secret` so it is also hidden in the top-level substitutions dump. The gateway URL rejects user information, query strings and fragments so secrets cannot be smuggled into a logged URL.

## Factory onboarding procedure

### 1. Build and flash the universal image

Before release, override `dashboard_import_url` with the public immutable package tag. From a repository checkout:

```sh
uvx --from esphome==2026.6.0 esphome config firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.0 esphome compile firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.0 esphome run firmware/hermes-voice-pe.factory.yaml
```

The same binary can be flashed to multiple devices. `name_add_mac_suffix: true` gives each factory node a unique mDNS name. It embeds no Wi-Fi, Native API, OTA, Worker or provider credential.

### 2. Provision Wi-Fi

In Home Assistant, select the discovered **hermes-vpe – Improv via BLE** device, provide the 2.4 GHz Wi-Fi credentials, then press the Voice PE center button to authorize. A phone can perform the equivalent flow directly. Improv Serial over USB is the independent provisioning and recovery path.

On successful association, firmware waits five seconds so Improv can return its result, disables BLE, waits for shutdown, and only then permits realtime wake/audio work. On Wi-Fi loss it stops any turn and wake inference before re-enabling BLE. ESPHome warns that BLE plus audio is resource intensive; this gated lifecycle follows [ESPHome's Improv guidance](https://esphome.io/components/esp32_improv/) and the official device behavior.

### 3. Add and adopt

Home Assistant discovers the ESPHome Native API node over mDNS. Add it to Home Assistant, then select **Adopt** in ESPHome Device Builder. Dashboard Import creates adopter-owned YAML with the unique node name, Wi-Fi secret references and a generated Native API encryption key, then installs the public package over native OTA.

The first adopted build is intentionally gateway-unconfigured. Adoption must succeed without Hermes, ElevenLabs, Cloudflare or project-specific secrets.

The universal factory API is necessarily keyless so an unknown future owner can make this first handoff. Keep the unit on a trusted or isolated provisioning LAN. Once the first adopted image is installed, its generated key encrypts the Native API; the remaining temporary exposure is passwordless native OTA until the enrollment step below.

### 4. Enroll the gateway

Read **Hermes Device ID** in Home Assistant or the ESPHome logs. Add that ID and a fresh random token to the Worker's `DEVICE_TOKENS_JSON`. In the YAML generated by Device Builder, add:

```yaml
substitutions:
  # Keep the generated name/friendly_name substitutions too.
  device_id: ""  # hardware-derived default; normally leave empty
  hermes_gateway_url: !secret hermes_gateway_url
  hermes_device_token: !secret hermes_device_token
  ota_password: !secret ota_password
```

Add the three values to that Device Builder installation's `secrets.yaml`, validate, and install over OTA. Use a fresh high-entropy OTA password. The URL must be a credential-free `wss://.../v2/realtime` URL; the token must match the Worker's entry for the displayed hardware ID.

ESPHome Dashboard Import generates a unique Native API key but does not generate an OTA password. The universal factory and first imported build therefore have a short, intentional passwordless native-OTA handoff so Device Builder can take ownership. Make this hardening/enrollment install immediately; the currently running blank-OTA image accepts it, and every later native OTA requires the new password. This limitation is in ESPHome's adoption materializer, not the Voice PE hardware.

Changing the ESPHome node or friendly name does not change the default hardware-derived gateway identity, Durable Object owner or conversation lane. An explicit `device_id` override is supported for migrations but must be stable and unique.

## Ongoing ESPHome compatibility

The adopted production package intentionally retains:

- encrypted Native API discovery and entity management;
- Home Assistant media-player/music and announcement support;
- ESPHome logs over API or USB;
- password-protected native ESPHome OTA and OTA safe mode after enrollment;
- Dashboard Import project metadata;
- Improv BLE and Improv Serial recovery;
- the physical 10-second factory-reset gesture.

BLE is disabled during normal connected operation. If Wi-Fi is lost, it is re-enabled and Improv begins after its normal Wi-Fi timeout. This preserves recovery without keeping BLE beside realtime voice.

Safe mode deliberately exposes only its reduced recovery surface—Wi-Fi, serial logging and OTA—not Hermes or the normal Native API application. A failed gateway must never cause an ESPHome reboot: `api.reboot_timeout: 0s` keeps Hermes voice independent from HA, while gateway failure remains isolated from API, OTA and HA media.

## Reset and decommissioning

Factory reset erases mutable ESPHome preferences and credentials learned dynamically by the factory image. It cannot remove secrets compiled into a later adopted binary. If an adopted YAML contains Wi-Fi or Worker values, secure decommissioning requires erasing/reflashing the zero-secret factory image, not only pressing the reset gesture.

Publish the matching WebSerial/factory binary alongside every release so a failed adoption, network change or interrupted configuration always has a USB recovery path. The firmware deliberately has no fallback access point or captive portal: ESPHome's captive portal automatically enables browser-based OTA, and an unauthenticated factory AP would make that recovery surface unsafe.

## Release-blocking verification

- Fetch the exact tagged `dashboard_import_url` anonymously and clone the exact custom-component ref; reject `@main`, missing files and local component paths in release builds.
- Use ESPHome's real Dashboard Import materializer in an empty directory, then validate and compile the generated YAML without adding Hermes settings.
- Compile factory, imported-unconfigured and gateway-configured variants with ESPHome 2026.6.0 and the current supported patch.
- Prove an empty/empty gateway pair compiles and remains inactive, a partial pair fails validation, a valid pair compiles, and ordinary `!secret` configuration output does not reveal the device token.
- On at least two erased Voice PEs, provision one through an active HA Bluetooth proxy and one through Improv Serial; verify unique names and center-button authorization.
- Adopt without editing the generated YAML; verify first OTA, generated API encryption, logs, HA media and diagnostics before adding the gateway.
- Add the Worker URL/token and a random OTA password, then OTA again; verify later OTA rejects a missing/wrong password, voice becomes ready without USB, and the hardware device ID survives rename, reboot and later OTA.
- Run gateway DNS/TLS/401/outage faults while repeatedly exercising API, logs, HA media and OTA; require no reboot, reconnect storm or resource trend.
- Test power loss during OTA, forced safe mode, Wi-Fi-loss BLE recovery, factory-image USB recovery and secure erase/reflash.
