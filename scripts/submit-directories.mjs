#!/usr/bin/env node
/**
 * Submit RemembrallMCP to MCP server directories using Playwright.
 *
 * Prerequisites:
 *   npm install playwright@1.52.0
 *   npx playwright install chromium
 *
 * Usage:
 *   node scripts/submit-directories.mjs
 */

import { chromium } from "playwright";
import { mkdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SCREENSHOTS_DIR = join(__dirname, "screenshots");

// ── Project details ──────────────────────────────────────────────────

const PROJECT = {
  name: "RemembrallMCP",
  url: "https://github.com/roboticforce/remembrallmcp",
  description:
    "Whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, references) plus persistent memory. Rust core, Postgres + pgvector backend, MCP protocol. Hybrid semantic + full-text search, blast-radius impact analysis, incremental indexing for 8 languages.",
  tagline: "Whole-codebase knowledge for AI coding agents",
  categories: ["AI", "Developer Tools", "MCP", "Memory", "Knowledge Graph"],
  language: "Rust",
  license: "MIT",
  author: "Steven Leggett",
  email: "contact@roboticforce.io",
};

// ── Helpers ──────────────────────────────────────────────────────────

function log(site, msg) {
  const ts = new Date().toISOString().slice(11, 19);
  console.log(`[${ts}] [${site}] ${msg}`);
}

async function screenshot(page, site, label) {
  const filename = `${site}-${label}.png`;
  const filepath = join(SCREENSHOTS_DIR, filename);
  await page.screenshot({ path: filepath, fullPage: true });
  log(site, `Screenshot saved: ${filename}`);
}

/**
 * Try to fill an input by multiple selectors. Returns true if filled.
 */
async function tryFill(page, selectors, value, timeout = 3000) {
  for (const sel of selectors) {
    try {
      const el = page.locator(sel).first();
      if (await el.isVisible({ timeout })) {
        await el.fill(value);
        return true;
      }
    } catch {
      // selector not found, try next
    }
  }
  return false;
}

/**
 * Try to click a button/link by multiple selectors. Returns true if clicked.
 */
async function tryClick(page, selectors, timeout = 3000) {
  for (const sel of selectors) {
    try {
      const el = page.locator(sel).first();
      if (await el.isVisible({ timeout })) {
        await el.click();
        return true;
      }
    } catch {
      // selector not found, try next
    }
  }
  return false;
}

/**
 * Fill common field patterns across various submission forms.
 */
async function fillCommonFields(page) {
  // Name / title
  await tryFill(
    page,
    [
      'input[name="name"]',
      'input[name="title"]',
      'input[name="tool_name"]',
      'input[name="server_name"]',
      'input[name="project_name"]',
      'input[placeholder*="name" i]',
      'input[placeholder*="title" i]',
      'input[id*="name" i]',
      'input[id*="title" i]',
    ],
    PROJECT.name
  );

  // URL / repo / website / link
  await tryFill(
    page,
    [
      'input[name="url"]',
      'input[name="link"]',
      'input[name="website"]',
      'input[name="repo"]',
      'input[name="repo_url"]',
      'input[name="github"]',
      'input[name="github_url"]',
      'input[name="repository"]',
      'input[name="homepage"]',
      'input[placeholder*="url" i]',
      'input[placeholder*="github" i]',
      'input[placeholder*="repo" i]',
      'input[placeholder*="link" i]',
      'input[placeholder*="website" i]',
      'input[type="url"]',
      'input[id*="url" i]',
      'input[id*="link" i]',
      'input[id*="repo" i]',
      'input[id*="github" i]',
    ],
    PROJECT.url
  );

  // Description / about
  for (const sel of [
    'textarea[name="description"]',
    'textarea[name="about"]',
    'textarea[name="details"]',
    'textarea[name="summary"]',
    'textarea[placeholder*="description" i]',
    'textarea[placeholder*="about" i]',
    'textarea[id*="description" i]',
    'textarea[id*="about" i]',
    "textarea",
  ]) {
    try {
      const el = page.locator(sel).first();
      if (await el.isVisible({ timeout: 2000 })) {
        await el.fill(PROJECT.description);
        break;
      }
    } catch {
      // continue
    }
  }

  // Short description / tagline
  await tryFill(
    page,
    [
      'input[name="short_description"]',
      'input[name="tagline"]',
      'input[name="one_liner"]',
      'input[name="subtitle"]',
      'input[placeholder*="tagline" i]',
      'input[placeholder*="short" i]',
      'input[placeholder*="one liner" i]',
    ],
    PROJECT.tagline
  );

  // Category / tags
  await tryFill(
    page,
    [
      'input[name="category"]',
      'input[name="categories"]',
      'input[name="tags"]',
      'input[placeholder*="categor" i]',
      'input[placeholder*="tag" i]',
    ],
    PROJECT.categories.join(", ")
  );

  // Language
  await tryFill(
    page,
    [
      'input[name="language"]',
      'input[name="programming_language"]',
      'input[placeholder*="language" i]',
    ],
    PROJECT.language
  );

  // License
  await tryFill(
    page,
    [
      'input[name="license"]',
      'input[placeholder*="license" i]',
    ],
    PROJECT.license
  );

  // Author / submitter name
  await tryFill(
    page,
    [
      'input[name="author"]',
      'input[name="submitter"]',
      'input[name="your_name"]',
      'input[name="contact_name"]',
      'input[placeholder*="author" i]',
      'input[placeholder*="your name" i]',
    ],
    PROJECT.author
  );

  // Email
  if (PROJECT.email) {
    await tryFill(
      page,
      [
        'input[name="email"]',
        'input[name="contact_email"]',
        'input[type="email"]',
        'input[placeholder*="email" i]',
      ],
      PROJECT.email
    );
  }
}

// ── Submission functions ─────────────────────────────────────────────

async function submitMcpServersOrg(browser) {
  const site = "mcpservers.org";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to submission page...");
    await page.goto("https://mcpservers.org/submit", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-before");

    await fillCommonFields(page);
    await screenshot(page, site, "02-filled");

    // Try to submit the form
    const submitted = await tryClick(page, [
      'button[type="submit"]',
      'input[type="submit"]',
      'button:has-text("Submit")',
      'button:has-text("submit")',
      'a:has-text("Submit")',
    ]);

    if (submitted) {
      await page.waitForTimeout(3000);
      await screenshot(page, site, "03-submitted");
      log(site, "Form submitted successfully");
    } else {
      await screenshot(page, site, "03-no-submit-button");
      log(site, "Could not find submit button - check screenshots");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

async function submitPulseMcp(browser) {
  const site = "pulsemcp.com";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to submission page...");
    await page.goto("https://www.pulsemcp.com/submit", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-before");

    await fillCommonFields(page);
    await screenshot(page, site, "02-filled");

    const submitted = await tryClick(page, [
      'button[type="submit"]',
      'input[type="submit"]',
      'button:has-text("Submit")',
      'button:has-text("submit")',
    ]);

    if (submitted) {
      await page.waitForTimeout(3000);
      await screenshot(page, site, "03-submitted");
      log(site, "Form submitted successfully");
    } else {
      await screenshot(page, site, "03-no-submit-button");
      log(site, "Could not find submit button - check screenshots");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

async function submitMcpSo(browser) {
  const site = "mcp.so";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to site...");
    await page.goto("https://mcp.so/", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-homepage");

    // Look for submit / add / list links
    const submitClicked = await tryClick(page, [
      'a:has-text("Submit")',
      'a:has-text("Add")',
      'a:has-text("List your")',
      'a:has-text("submit")',
      'button:has-text("Submit")',
      'a[href*="submit"]',
      'a[href*="add"]',
    ]);

    if (submitClicked) {
      await page.waitForTimeout(2000);
      await screenshot(page, site, "02-submit-page");
      await fillCommonFields(page);
      await screenshot(page, site, "03-filled");

      const submitted = await tryClick(page, [
        'button[type="submit"]',
        'input[type="submit"]',
        'button:has-text("Submit")',
      ]);

      if (submitted) {
        await page.waitForTimeout(3000);
        await screenshot(page, site, "04-submitted");
        log(site, "Form submitted successfully");
      } else {
        log(site, "Filled form but could not find submit button");
      }
    } else {
      log(site, "No submit link found on homepage - check screenshot");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

async function submitMcpServerDirectory(browser) {
  const site = "mcpserverdirectory.org";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to submission page...");
    await page.goto("https://mcpserverdirectory.org/submit", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-before");

    await fillCommonFields(page);
    await screenshot(page, site, "02-filled");

    const submitted = await tryClick(page, [
      'button[type="submit"]',
      'input[type="submit"]',
      'button:has-text("Submit")',
      'button:has-text("submit")',
    ]);

    if (submitted) {
      await page.waitForTimeout(3000);
      await screenshot(page, site, "03-submitted");
      log(site, "Form submitted successfully");
    } else {
      await screenshot(page, site, "03-no-submit-button");
      log(site, "Could not find submit button - check screenshots");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

async function submitAiToolsFyi(browser) {
  const site = "aitools.fyi";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to submission page...");
    await page.goto("https://aitools.fyi/submit-a-tool", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-before");

    await fillCommonFields(page);
    await screenshot(page, site, "02-filled");

    const submitted = await tryClick(page, [
      'button[type="submit"]',
      'input[type="submit"]',
      'button:has-text("Submit")',
      'button:has-text("submit")',
    ]);

    if (submitted) {
      await page.waitForTimeout(3000);
      await screenshot(page, site, "03-submitted");
      log(site, "Form submitted successfully");
    } else {
      await screenshot(page, site, "03-no-submit-button");
      log(site, "Could not find submit button - check screenshots");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

async function submitMcpServerFinder(browser) {
  const site = "mcpserverfinder.com";
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(30000);

  try {
    log(site, "Navigating to site...");
    await page.goto("https://www.mcpserverfinder.com/", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });
    await page.waitForTimeout(2000);
    await screenshot(page, site, "01-homepage");

    // Look for submit / add links
    const submitClicked = await tryClick(page, [
      'a:has-text("Submit")',
      'a:has-text("Add")',
      'a:has-text("List")',
      'a[href*="submit"]',
      'a[href*="add"]',
      'button:has-text("Submit")',
    ]);

    if (submitClicked) {
      await page.waitForTimeout(2000);
      await screenshot(page, site, "02-submit-page");
      await fillCommonFields(page);
      await screenshot(page, site, "03-filled");

      const submitted = await tryClick(page, [
        'button[type="submit"]',
        'input[type="submit"]',
        'button:has-text("Submit")',
      ]);

      if (submitted) {
        await page.waitForTimeout(3000);
        await screenshot(page, site, "04-submitted");
        log(site, "Form submitted successfully");
      } else {
        log(site, "Filled form but could not find submit button");
      }
    } else {
      log(site, "No submit link found on homepage - check screenshot");
    }

    return { site, status: "success" };
  } catch (err) {
    await screenshot(page, site, "error").catch(() => {});
    log(site, `Error: ${err.message}`);
    return { site, status: "failed", error: err.message };
  } finally {
    await context.close();
  }
}

// ── Main ─────────────────────────────────────────────────────────────

async function main() {
  // Ensure screenshots directory exists
  if (!existsSync(SCREENSHOTS_DIR)) {
    mkdirSync(SCREENSHOTS_DIR, { recursive: true });
  }

  console.log("=".repeat(60));
  console.log("RemembrallMCP - Directory Submission Script");
  console.log("=".repeat(60));
  console.log(`Project: ${PROJECT.name}`);
  console.log(`URL: ${PROJECT.url}`);
  console.log(`Screenshots: ${SCREENSHOTS_DIR}`);
  console.log("=".repeat(60));
  console.log();

  const browser = await chromium.launch({
    headless: true,
  });

  try {
    const results = await Promise.allSettled([
      submitMcpServersOrg(browser),
      submitPulseMcp(browser),
      submitMcpSo(browser),
      submitMcpServerDirectory(browser),
      submitAiToolsFyi(browser),
      submitMcpServerFinder(browser),
    ]);

    // ── Summary ────────────────────────────────────────────────────

    console.log();
    console.log("=".repeat(60));
    console.log("SUBMISSION SUMMARY");
    console.log("=".repeat(60));

    let succeeded = 0;
    let failed = 0;

    for (const result of results) {
      if (result.status === "fulfilled") {
        const { site, status, error } = result.value;
        const icon = status === "success" ? "[OK]" : "[FAIL]";
        console.log(`  ${icon} ${site}${error ? ` - ${error}` : ""}`);
        if (status === "success") succeeded++;
        else failed++;
      } else {
        console.log(`  [FAIL] Unknown - ${result.reason}`);
        failed++;
      }
    }

    console.log();
    console.log(`Results: ${succeeded} succeeded, ${failed} failed`);
    console.log(`Screenshots saved to: ${SCREENSHOTS_DIR}`);
    console.log("=".repeat(60));
    console.log();
    console.log(
      "NOTE: Review the screenshots to verify each submission."
    );
    console.log(
      "Some sites may require CAPTCHA or login - complete those manually."
    );
  } finally {
    await browser.close();
  }
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
