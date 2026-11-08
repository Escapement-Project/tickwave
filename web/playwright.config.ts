import { defineConfig, devices } from "@playwright/test";

const PORT = 8124;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  reporter: [["list"]],
  timeout: 60_000,
  use: { baseURL: `http://127.0.0.1:${PORT}` },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"], deviceScaleFactor: 2 } },
    { name: "firefox", use: { ...devices["Desktop Firefox"], deviceScaleFactor: 2 } },
  ],
  webServer: {
    command: `node build.ts --serve ${PORT}`,
    url: `http://127.0.0.1:${PORT}/dev/`,
    reuseExistingServer: false,
  },
});
