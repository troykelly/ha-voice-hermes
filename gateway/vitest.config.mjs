import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest({
      miniflare: {
        outboundService: "provider-mock",
        serviceBindings: { PROVIDER_MOCK: "provider-mock" },
        workers: [
          {
            name: "provider-mock",
            compatibilityDate: "2026-07-11",
            modules: true,
            routes: ["api.elevenlabs.io/*", "hermes.example.com/*"],
            scriptPath: "./test/provider-mock.mjs",
          },
        ],
      },
      wrangler: { configPath: "./wrangler.test.toml" },
    }),
  ],
  test: {
    testTimeout: 20_000,
  },
});
