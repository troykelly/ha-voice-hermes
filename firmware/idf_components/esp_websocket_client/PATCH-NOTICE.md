# esp_websocket_client security patch

This directory vendors Espressif `esp_websocket_client` 1.7.0 from
`esp-protocols` commit `b385915ca011094238f5e8ebc45d539183b09cf2` under
Apache License 2.0.

The only functional change sets `WS_TRANSPORT_REDIRECT_HEADER_SUPPORT` to
zero. A WebSocket HTTP `3xx` therefore fails its connection epoch rather than
following `Location` with the configured device `Authorization` and
`X-Device-ID` headers. Operators must configure the final canonical `wss://`
Worker endpoint; redirects are intentionally unsupported.

When updating the upstream component, retain this fail-closed property and add
a redirect regression check to the physical/device release gate.
