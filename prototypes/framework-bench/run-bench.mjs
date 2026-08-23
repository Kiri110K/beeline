// Headless WebKit runner: serves each built app, runs its self-driving
// benchmark, and prints the collected metrics as JSON.
import { spawn } from "node:child_process";
import { webkit } from "playwright";

const APPS = [
  { name: "react", dir: "react-app/dist", port: 8471 },
  { name: "solid", dir: "solid-app/dist", port: 8472 },
];

async function runApp(app, browser) {
  const server = spawn("python3", ["-m", "http.server", String(app.port), "-d", app.dir], {
    stdio: "ignore",
  });
  try {
    await new Promise((r) => setTimeout(r, 800));
    const page = await browser.newPage({ viewport: { width: 960, height: 700 } });
    await page.goto(`http://127.0.0.1:${app.port}/`);
    await page.waitForFunction("window.__BENCH_DONE === true", null, { timeout: 180000 });
    const results = await page.evaluate("window.__BENCH_RESULTS");
    await page.close();
    return results;
  } finally {
    server.kill();
  }
}

const browser = await webkit.launch({ headless: true });
const out = {};
for (const app of APPS) {
  for (let round = 1; round <= 3; round++) {
    const r = await runApp(app, browser);
    out[`${app.name}#${round}`] = r;
    console.error(`${app.name} round ${round} done`);
  }
}
await browser.close();
console.log(JSON.stringify(out, null, 1));
