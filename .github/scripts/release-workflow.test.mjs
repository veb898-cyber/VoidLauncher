import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const publish = readFileSync(
  new URL("../workflows/publish.yml", import.meta.url),
  "utf8",
);
const ci = readFileSync(new URL("../workflows/ci.yml", import.meta.url), "utf8");
const publishScript = readFileSync(
  new URL("./publish-latest-json.mjs", import.meta.url),
  "utf8",
);
const manifestScript = readFileSync(
  new URL("./latest-json.mjs", import.meta.url),
  "utf8",
);

test("release workflow never pushes to main", () => {
  assert.ok(!publish.includes("git push"), "git push must be gone entirely");
  assert.ok(!publish.includes("HEAD:main"));
  assert.ok(!publish.includes("refs/heads/main"));
  assert.ok(!publish.includes("--force"));
  assert.ok(!publish.includes("push origin"));
});

test("release workflow uses the GitHub App token with minimal permissions", () => {
  assert.match(publish, /actions\/create-github-app-token@v3/);
  assert.match(publish, /app-id:\s*\$\{\{\s*secrets\.RELEASE_APP_ID\s*\}\}/);
  assert.match(
    publish,
    /private-key:\s*\$\{\{\s*secrets\.RELEASE_APP_PRIVATE_KEY\s*\}\}/,
  );
  assert.match(publish, /permission-contents:\s*write/);
  assert.match(publish, /permission-pull-requests:\s*write/);
  assert.match(publish, /check-release-app-secrets\.mjs/);
  assert.match(publish, /publish-latest-json\.mjs/);
  assert.match(publish, /release\/latest-json-v/);
});

test("the token is created only after the installer was signed", () => {
  const signIndex = publish.indexOf("Sign installer and generate latest.json");
  const tokenIndex = publish.indexOf("Create GitHub App token");
  const publishIndex = publish.indexOf("Publish latest.json via pull request");
  assert.ok(signIndex > 0, "signing step must exist");
  assert.ok(tokenIndex > signIndex, "token must be requested after signing");
  assert.ok(publishIndex > tokenIndex, "PR publication must happen after signing");
});

test("release workflow does not echo private material", () => {
  assert.ok(!/echo[^\n]*RELEASE_APP_PRIVATE_KEY/.test(publish));
  assert.ok(
    !/\becho[^\n]*\$\{?(?:TAURI_SIGNING_PRIVATE_KEY|TAURI_SIGNING_PRIVATE_KEY_PASSWORD|RELEASE_APP_PRIVATE_KEY|KEY_FOR_CLI)\b/.test(
      publish,
    ),
    "secret variables must never be echoed",
  );
  assert.ok(!/set\s+-[a-z]*x/.test(publish), "do not enable shell xtrace");
  assert.ok(!/\bghp_[A-Za-z0-9]{20,}/.test(publish));
  assert.ok(!/\bgithub_pat_[A-Za-z0-9_]{20,}/.test(publish));
});

test("manifest script supports generation, validation, and dry-run", () => {
  assert.match(manifestScript, /export function buildManifest/);
  assert.match(manifestScript, /export function validateManifest/);
  assert.match(manifestScript, /--dry-run|\["dry-run"\]/);
  assert.match(publishScript, /dryRun/);
  assert.match(publishScript, /--dry-run|\["dry-run"\]/);
});

test("CI runs the release script tests", () => {
  assert.match(ci, /node --test/);
  for (const file of [
    "latest-json.test.mjs",
    "publish-latest-json.test.mjs",
    "check-release-app-secrets.test.mjs",
    "release-workflow.test.mjs",
  ]) {
    assert.ok(ci.includes(file), `CI must run ${file}`);
  }
});
