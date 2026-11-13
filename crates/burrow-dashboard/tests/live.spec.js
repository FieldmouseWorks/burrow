// The live view at a laptop and a phone: `render-live` (examples/) writes
// the page for tests/stall-2026-11-11.json (three parked slots) beside the
// shell, and this renders it at 1920x1080 and on an iPhone 16 Pro Max.
// Run: SHELL_DIR=assets PAGE=out/live.html SHOTS=out npx playwright test tests/live.spec.js
const { test, expect } = require('@playwright/test');

// iPhone 16 Pro Max: 440x956 CSS pixels at 3x.
const iphone16ProMax = { viewport: { width: 440, height: 956 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true };
test.use({ launchOptions: { executablePath: process.env.CHROMIUM_EXECUTABLE_PATH } });

const http = require('http');
const fs = require('fs');
const path = require('path');

// The page as the service would send it, and the shell under /assets/VERSION/.
let server;
let page_url;
test.beforeAll(async () => {
  server = http.createServer((req, res) => {
    const m = req.url.match(/^\/assets\/[^/]+\/(.+)$/);
    const file = m ? path.join(process.env.SHELL_DIR, m[1].replace(/\.\./g, '')) : process.env.PAGE;
    fs.readFile(file, (err, body) => {
      const types = { '.css': 'text/css', '.js': 'text/javascript', '.png': 'image/png', '.woff2': 'font/woff2' };
      res.writeHead(err ? 404 : 200, { 'content-type': types[path.extname(file)] || 'text/html' });
      res.end(body);
    });
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  page_url = `http://127.0.0.1:${server.address().port}/`;
});
test.afterAll(() => server.close());
const shots = process.env.SHOTS;
const order = ['front', 'slots', 'queue', 'deploy', 'meters', 'holds'];

async function ids(page) {
  return page.$$eval('.live h2', (hs) => hs.map((h) => h.id));
}

test('laptop: queue and slots side by side', async ({ browser }) => {
  const ctx = await browser.newContext({ viewport: { width: 1920, height: 1080 } });
  const page = await ctx.newPage();
  await page.goto(page_url);
  await page.evaluate(() => document.fonts.ready);
  expect(await ids(page)).toEqual(order);
  expect(await page.locator('.slot.stalled').count()).toBe(3);
  const slots = await page.locator('section.slots').boundingBox();
  const queue = await page.locator('section.queue').boundingBox();
  expect(queue.x).toBeGreaterThan(slots.x + slots.width - 1);
  expect(Math.abs(queue.y - slots.y)).toBeLessThan(2);
  await page.screenshot({ path: `${shots}/live-1920x1080.png`, fullPage: true });
  await ctx.close();
});

test('phone: one column', async ({ browser }) => {
  const ctx = await browser.newContext(iphone16ProMax);
  const page = await ctx.newPage();
  await page.goto(page_url);
  await page.evaluate(() => document.fonts.ready);
  expect(await ids(page)).toEqual(order);
  await expect(page.locator('.slot[data-state="parked"]')).toHaveCount(3);
  await expect(page.locator('.meters')).toContainText('fixture-worker / FieldmouseWorks');
  const slots = await page.locator('section.slots').boundingBox();
  const queue = await page.locator('section.queue').boundingBox();
  expect(queue.y).toBeGreaterThan(slots.y + slots.height - 1);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: `${shots}/live-iphone-16-pro-max.png`, fullPage: true });
  await ctx.close();
});
