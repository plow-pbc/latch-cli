// One phone link, in WebKit as an iPhone: open it, type `values` into the
// host's form by field name, save, and open the link again, which must
// answer that it was used. Screenshots go to OUT-*.png.
// Usage: node phone.mjs LINK.json VALUES.json OUT
import { webkit, devices } from "playwright";
import { readFileSync } from "fs";

const [linkFile, valuesFile, out] = process.argv.slice(2);
const url = JSON.parse(readFileSync(linkFile, "utf8")).url;
const values = JSON.parse(readFileSync(valuesFile, "utf8"));

const browser = await webkit.launch();
const context = await browser.newContext({ ...devices["iPhone 15"] });
try {
  const page = await context.newPage();
  await page.goto(url);
  const form = page.frameLocator("#page");
  await form.locator("button").waitFor({ timeout: 30000 });
  console.log("header:", await page.textContent("#status"));
  console.log("site:", await form.locator("#site").textContent());
  for (const [label, value] of Object.entries(values)) {
    await form.locator(`[name="${label}"]`).fill(value);
  }
  await page.screenshot({ path: `${out}-1-form.png` });
  await form.locator("button").click();
  await form.locator("#result").waitFor({ state: "visible", timeout: 30000 });
  const result = await form.locator("#result").textContent();
  console.log("result:", result);
  await page.screenshot({ path: `${out}-2-result.png` });
  if (!result.startsWith("Saved on ")) throw new Error(`not saved: ${result}`);

  const again = await context.newPage();
  await again.goto(url);
  await again.waitForFunction(() => document.getElementById("status").className === "bad", null, { timeout: 30000 });
  const header = await again.textContent("#status");
  console.log("opened again:", header);
  await again.screenshot({ path: `${out}-3-again.png` });
  if (!header.includes("already been used")) throw new Error(`reused link: ${header}`);
} finally {
  await browser.close();
}
