# Vendored microWakeWord models

These unmodified artifacts are vendored to make released firmware
reproducible. They are distributed under Apache License 2.0; see
[`../../LICENSES/Apache-2.0.txt`](../../LICENSES/Apache-2.0.txt).

- `okay_nabu.json` and `okay_nabu.tflite`: Kevin Ahrendt's Okay Nabu
  `okay_nabu_20241226.3` release artifact from
  `kahrendt/microWakeWord` tag commit
  `4665173cd35f1cff9a61e06fc427f124766c488e`.
- `vad.json` and `vad.tflite`: Kevin Ahrendt's ESPHome V2 VAD artifacts,
  captured from `esphome/micro-wake-word-models` commit
  `05b65922cc433c9df13e98e32a7fe520758c837e`.

`SHA256SUMS` is the release integrity manifest. Do not replace a model in an
existing release tag. A model update requires reviewed hashes, physical wake
word acceptance testing, and a new repository release.
