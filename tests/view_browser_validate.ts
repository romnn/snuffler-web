// Check delayed activity feedback while a real worker answers a scroll or pan.
import assert from "node:assert/strict";
import {
  appUrl,
  BOOT_TIMEOUT_MS,
  launchParityBrowser,
  waitForAppReady,
} from "./parity/parity_browser";

const browser = await launchParityBrowser();
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.setDefaultTimeout(BOOT_TIMEOUT_MS);
  await page.route("**/snuffler_bridge/service.py", async (route) => {
    const response = await route.fetch();
    const fixture = `
_loading_get_view = BridgeServicer.get_view
async def _test_loading_get_view(self, request, data):
    import asyncio
    await operation.await_external(asyncio.sleep(0.9))
    return await _loading_get_view(self, request, data)
BridgeServicer.get_view = _test_loading_get_view
`;
    await route.fulfill({ response, body: (await response.text()) + fixture });
  });
  const loaded = await page.goto(appUrl({ a11y: "pointer", example: "eifel" }));
  assert.ok(loaded?.ok());
  await waitForAppReady(page);
  const loading = page.getByText("Updating view", { exact: true });
  const badge = page.getByText("Updating traces…", { exact: true });
  await loading.waitFor({ state: "hidden" });
  await page.mouse.move(1000, 400);
  let started = Date.now();
  await page.mouse.wheel(0, 600);
  await loading.waitFor({ state: "visible", timeout: 5000 });
  assert.ok(Date.now() - started >= 400, "activity feedback must wait through its grace period");
  assert.equal(await badge.count(), 0, "the canvas must not duplicate activity feedback");
  await loading.waitFor({ state: "hidden" });
  console.log("ok   scrolling uses delayed activity feedback without a canvas badge");
  await page.mouse.move(1050, 400);
  started = Date.now();
  await page.mouse.down();
  await page.mouse.move(650, 400, { steps: 4 });
  await page.mouse.up();
  await loading.waitFor({ state: "visible", timeout: 5000 });
  assert.ok(Date.now() - started >= 400, "activity feedback must wait through its grace period");
  assert.equal(await badge.count(), 0, "the canvas must not duplicate activity feedback");
  await loading.waitFor({ state: "hidden" });
  console.log("ok   panning uses delayed activity feedback without a canvas badge");
} finally {
  await browser.close();
}
