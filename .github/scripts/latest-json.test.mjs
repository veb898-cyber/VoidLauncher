import { test } from "node:test";
import assert from "node:assert/strict";

import {
  assertNoSecrets,
  buildManifest,
  compareVersions,
  parseTag,
  parseVersion,
  serializeManifest,
  validateInstallerUrl,
  validateManifest,
} from "./latest-json.mjs";

const TAG = "v1.6.0";
const REPO = "veb898-cyber/VoidLauncher";
const INSTALLER = "VoidLauncher_1.6.0_x64-setup.exe";
const PUB_DATE = "2026-11-15T12:00:00Z";
const SIGNATURE = Buffer.from(
  "voidlauncher test installer signature ".repeat(8),
).toString("base64");

function validManifest(overrides = {}) {
  return buildManifest({
    tag: TAG,
    repository: REPO,
    installer: INSTALLER,
    signature: SIGNATURE,
    notes: "Release v1.6.0",
    pubDate: PUB_DATE,
    ...overrides,
  });
}

test("builds a Tauri-compatible manifest with the real release URL", () => {
  const manifest = validManifest();
  assert.equal(manifest.version, "1.6.0");
  assert.equal(manifest.notes, "Release v1.6.0");
  assert.equal(manifest.pub_date, PUB_DATE);
  assert.deepEqual(Object.keys(manifest.platforms), ["windows-x86_64"]);
  assert.equal(
    manifest.platforms["windows-x86_64"].url,
    "https://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/VoidLauncher_1.6.0_x64-setup.exe",
  );
  assert.equal(manifest.platforms["windows-x86_64"].signature, SIGNATURE);
});

test("generation is deterministic for the same inputs", () => {
  assert.equal(
    serializeManifest(validManifest()),
    serializeManifest(validManifest()),
  );
});

test("rejects malformed release tags", () => {
  for (const tag of ["1.6.0", "v1.6", "v01.6.0", "v1.06.0", "v1.6.0/../x"]) {
    assert.throws(() => parseTag(tag), /tag|semantic/i, tag);
  }
});

test("rejects invalid SemVer prerelease and build identifiers", () => {
  for (const tag of [
    "v1.6.0-01",
    "v1.6.0-00",
    "v1.6.0-alpha.01",
    "v1.6.0-",
    "v1.6.0-.",
    "v1.6.0-alpha..beta",
    "v1.6.0-.alpha",
    "v1.6.0-alpha.",
    "v1.6.0+",
    "v1.6.0+.",
    "v1.6.0+build..meta",
    "v1.6.0-alpha+build..meta",
    "v1.6.0+build+meta",
    "v1.6.0-α",
  ]) {
    assert.throws(
      () => parseTag(tag),
      /semantic|prerelease|build/i,
      tag,
    );
  }
});

test("accepts valid SemVer prerelease and build metadata", () => {
  for (const tag of [
    "v1.6.0-0",
    "v1.6.0-0A",
    "v1.6.0-alpha-1",
    "v1.6.0-alpha.1",
    "v1.6.0+001",
    "v1.6.0-alpha+build.001",
  ]) {
    assert.doesNotThrow(() => parseTag(tag), tag);
  }
});

test("stores prerelease and build metadata without coercing core numbers", () => {
  const parsed = parseVersion("1.6.0-alpha.1+build.001");
  assert.deepEqual(parsed.core, ["1", "6", "0"]);
  assert.deepEqual(parsed.prerelease, ["alpha", "1"]);
  assert.deepEqual(parsed.build, ["build", "001"]);
});

test("rejects installers that do not belong to the tag version", () => {
  assert.throws(
    () =>
      buildManifest({
        tag: TAG,
        repository: REPO,
        installer: "VoidLauncher_1.5.0_x64-setup.exe",
        signature: SIGNATURE,
        pubDate: PUB_DATE,
      }),
    /version 1.6.0/,
  );
  assert.throws(
    () =>
      buildManifest({
        tag: TAG,
        repository: REPO,
        installer: "../VoidLauncher_1.6.0_x64-setup.exe",
        signature: SIGNATURE,
        pubDate: PUB_DATE,
      }),
    /Installer name/,
  );
});

test("rejects malformed signatures without echoing their contents", () => {
  for (const signature of ["", "short", "not base64 !!!", `${SIGNATURE} ${SIGNATURE}`]) {
    assert.throws(
      () =>
        buildManifest({
          tag: TAG,
          repository: REPO,
          installer: INSTALLER,
          signature,
          pubDate: PUB_DATE,
        }),
      /signature/i,
    );
  }
});

test("rejects foreign or non-https installer URLs", () => {
  for (const url of [
    "http://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/VoidLauncher_1.6.0_x64-setup.exe",
    "https://evil.example/veb898-cyber/VoidLauncher/releases/download/v1.6.0/VoidLauncher_1.6.0_x64-setup.exe",
    "https://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/other.exe",
    "https://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/VoidLauncher_1.6.0_x64-setup.exe?download=1",
    "https://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/../VoidLauncher_1.6.0_x64-setup.exe",
  ]) {
    assert.throws(
      () => validateInstallerUrl(url, REPO, TAG, INSTALLER),
      /URL/,
      url,
    );
  }
});

test("validates a round-tripped manifest and rejects tampering", () => {
  const manifest = validManifest();
  assert.doesNotThrow(() =>
    validateManifest(manifest, {
      tag: TAG,
      repository: REPO,
      installer: INSTALLER,
      signature: SIGNATURE,
    }),
  );

  const wrongSignature = structuredClone(manifest);
  wrongSignature.platforms["windows-x86_64"].signature = Buffer.from(
    "another signature ".repeat(10),
  ).toString("base64");
  assert.throws(
    () =>
      validateManifest(wrongSignature, {
        tag: TAG,
        repository: REPO,
        installer: INSTALLER,
        signature: SIGNATURE,
      }),
    /signature/,
  );

  const wrongUrl = structuredClone(manifest);
  wrongUrl.platforms["windows-x86_64"].url =
    "https://github.com/veb898-cyber/VoidLauncher/releases/download/v1.6.0/other.exe";
  assert.throws(
    () =>
      validateManifest(wrongUrl, {
        tag: TAG,
        repository: REPO,
        installer: INSTALLER,
      }),
    /URL/,
  );

  const extraField = structuredClone(manifest);
  extraField.secret = "must not be allowed";
  assert.throws(
    () =>
      validateManifest(extraField, {
        tag: TAG,
        repository: REPO,
        installer: INSTALLER,
      }),
    /unexpected fields/,
  );
});

test("compares versions with SemVer precedence", () => {
  const chain = [
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
  ];
  for (let i = 0; i < chain.length - 1; i += 1) {
    assert.equal(
      compareVersions(chain[i], chain[i + 1]),
      -1,
      `${chain[i]} < ${chain[i + 1]}`,
    );
  }
  assert.equal(compareVersions("1.6.0", "1.5.9"), 1);
  assert.equal(compareVersions("1.10.0", "1.9.9"), 1);
  assert.equal(compareVersions("1.6.0", "1.6.0"), 0);
  assert.equal(compareVersions("1.6.0-rc.1", "1.6.0"), -1);
  assert.equal(compareVersions("1.6.0-1", "1.6.0-alpha"), -1);
});

test("compares arbitrarily large numeric identifiers without precision loss", () => {
  // Number() would collapse these two 400-digit prerelease identifiers to
  // Infinity and report equality.
  const huge = "9".repeat(400);
  const hugeMinusOne = `${"9".repeat(399)}8`;
  assert.equal(compareVersions(`1.6.0-${huge}`, `1.6.0-${hugeMinusOne}`), 1);
  assert.equal(compareVersions(`1.6.0-${hugeMinusOne}`, `1.6.0-${huge}`), -1);

  // 9007199254740993 is above Number.MAX_SAFE_INTEGER; Number() rounds it to
  // 9007199254740992 and would report equality.
  assert.equal(
    compareVersions("1.6.0-9007199254740993", "1.6.0-9007199254740992"),
    1,
  );
  assert.equal(
    compareVersions("1.6.0-9007199254740992", "1.6.0-9007199254740993"),
    -1,
  );

  // Core numbers are also arbitrary precision in SemVer 2.0.0.
  const hugeCore = "9".repeat(400);
  const hugeCoreMinusOne = `${"9".repeat(399)}8`;
  assert.equal(
    compareVersions(`${hugeCore}.0.0`, `${hugeCoreMinusOne}.0.0`),
    1,
  );
});

test("ignores build metadata for precedence", () => {
  assert.equal(compareVersions("1.6.0+build.1", "1.6.0+build.2"), 0);
  assert.equal(compareVersions("1.6.0-alpha+001", "1.6.0-alpha+002"), 0);
  assert.equal(compareVersions("1.6.0-alpha+z", "1.6.0-beta+a"), -1);
});

test("secret scanning rejects values and patterns but never echoes them", () => {
  const secret = `ghp_${"x".repeat(36)}`;
  assert.throws(
    () => assertNoSecrets(`{"notes":"${secret}"}`),
    (error) => {
      assert.match(error.message, /GitHub token/);
      assert.ok(!error.message.includes(secret));
      return true;
    },
  );

  const envSecret = "private-key-value-1234567890";
  assert.throws(
    () =>
      assertNoSecrets(`{"notes":"${envSecret}"}`, {
        envNames: ["TAURI_SIGNING_PRIVATE_KEY"],
        env: { TAURI_SIGNING_PRIVATE_KEY: envSecret },
      }),
    (error) => {
      assert.match(error.message, /TAURI_SIGNING_PRIVATE_KEY/);
      assert.ok(!error.message.includes(envSecret));
      return true;
    },
  );

  const pemHeader = ["-----BEGIN", "PRIVATE KEY-----"].join(" ");
  assert.throws(
    () => assertNoSecrets(pemHeader),
    /PEM private key/,
  );
  assert.doesNotThrow(() =>
    assertNoSecrets(
      serializeManifest(validManifest()),
      { envNames: ["TAURI_SIGNING_PRIVATE_KEY"], env: {} },
    ),
  );
});
