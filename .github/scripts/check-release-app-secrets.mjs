#!/usr/bin/env node
// Validate that the release App credentials are present and look plausible
// before asking actions/create-github-app-token to use them. This script never
// prints the values (or their lengths): only generic diagnostics.

import { pathToFileURL } from "node:url";

export function checkReleaseAppSecrets(env = process.env) {
  const appId = String(env.RELEASE_APP_ID ?? "").trim();
  const privateKey = String(env.RELEASE_APP_PRIVATE_KEY ?? "").trim();

  if (!appId || !privateKey) {
    throw new Error(
      "RELEASE_APP_ID and RELEASE_APP_PRIVATE_KEY must both be configured",
    );
  }
  if (!/^\d+$/.test(appId)) {
    throw new Error("RELEASE_APP_ID must be the numeric GitHub App ID");
  }
  if (
    !privateKey.includes("BEGIN") ||
    !privateKey.includes("PRIVATE KEY") ||
    !privateKey.includes("END")
  ) {
    throw new Error(
      "RELEASE_APP_PRIVATE_KEY must be a PEM private key (BEGIN/END PRIVATE KEY)",
    );
  }
  return { appId };
}

const isMain =
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  try {
    checkReleaseAppSecrets();
    console.log("Release App secrets are present.");
  } catch (error) {
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
