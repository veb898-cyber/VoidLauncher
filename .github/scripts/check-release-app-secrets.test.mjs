import { test } from "node:test";
import assert from "node:assert/strict";

import { checkReleaseAppSecrets } from "./check-release-app-secrets.mjs";

const PEM_BEGIN = ["-----BEGIN", "RSA PRIVATE KEY-----"].join(" ");
const PEM_END = ["-----END", "RSA PRIVATE KEY-----"].join(" ");
const VALID_KEY = [
  PEM_BEGIN,
  "ZmFrZS1rZXktbWF0ZXJpYWw=",
  PEM_END,
].join("\n");

test("accepts present App credentials", () => {
  assert.deepEqual(
    checkReleaseAppSecrets({
      RELEASE_APP_ID: "123456",
      RELEASE_APP_PRIVATE_KEY: VALID_KEY,
    }),
    { appId: "123456" },
  );
});

test("rejects missing App credentials without printing them", () => {
  for (const env of [
    {},
    { RELEASE_APP_ID: "123456" },
    { RELEASE_APP_PRIVATE_KEY: VALID_KEY },
  ]) {
    assert.throws(
      () => checkReleaseAppSecrets(env),
      (error) => {
        assert.match(error.message, /RELEASE_APP_ID|RELEASE_APP_PRIVATE_KEY/);
        assert.ok(!error.message.includes(VALID_KEY));
        return true;
      },
    );
  }
});

test("rejects a non-numeric App ID", () => {
  assert.throws(
    () =>
      checkReleaseAppSecrets({
        RELEASE_APP_ID: "not-a-number",
        RELEASE_APP_PRIVATE_KEY: VALID_KEY,
      }),
    /numeric/,
  );
});

test("rejects a private key that is not PEM", () => {
  assert.throws(
    () =>
      checkReleaseAppSecrets({
        RELEASE_APP_ID: "123456",
        RELEASE_APP_PRIVATE_KEY: "not a PEM key",
      }),
    /PEM/,
  );
});

test("error messages never contain secret values", () => {
  const id = "999999";
  const key = `secret-material-${id}`;
  try {
    checkReleaseAppSecrets({
      RELEASE_APP_ID: id,
      RELEASE_APP_PRIVATE_KEY: key,
    });
    assert.fail("expected a rejection");
  } catch (error) {
    assert.ok(!error.message.includes(key));
    assert.ok(!error.message.includes(id));
  }
});
