import { test } from "node:test";
import assert from "node:assert/strict";

import {
  assertNoSecrets,
  buildManifest,
  compareVersions,
  parseTag,
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
  assert.equal(compareVersions("1.6.0", "1.5.9"), 1);
  assert.equal(compareVersions("1.10.0", "1.9.9"), 1);
  assert.equal(compareVersions("1.6.0", "1.6.0"), 0);
  assert.equal(compareVersions("1.6.0-rc.1", "1.6.0"), -1);
  assert.equal(compareVersions("1.6.0-rc.2", "1.6.0-rc.10"), -1);
  assert.equal(compareVersions("1.6.0-alpha", "1.6.0-beta"), -1);
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
