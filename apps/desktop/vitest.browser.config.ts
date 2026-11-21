// Real-DOM component tests (Milestone 9): the real components in a real browser engine (the
// installed Microsoft Edge, through Playwright; no browser download), with real key and mouse
// events, real focus and real form submission. jsdom tests cannot catch bugs in how the
// browser routes a submit (the nested-form bug of the wrong-password retry).
//   pnpm --dir apps/desktop test:browser
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  test: {
    include: ['src/**/*.browser.test.{ts,tsx}'],
    browser: {
      enabled: true,
      headless: true,
      provider: playwright({ launchOptions: { channel: 'msedge' } }),
      instances: [{ browser: 'chromium' }],
    },
  },
});
