import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Pure-Node Vitest (no Workers runtime). The backend modules under
    // test only use Web Crypto API + a small D1 surface — both are
    // mocked / available in modern Node, so we get test speed without
    // the miniflare overhead. If we ever need Workers-bound integration
    // tests we'll add @cloudflare/vitest-pool-workers as a second
    // project — the unit tests here stay fast.
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
});
