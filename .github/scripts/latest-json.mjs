#!/usr/bin/env node
// Generate and validate the Tauri updater manifest used by the release
// workflow. This script is deliberately dependency-free so it can run in CI
// and in local unit tests without a network or a Tauri installation.

import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const VERSION_RE =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$/;
const PUB_DATE_RE =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?Z$/;
const BASE64_RE = /^[A-Za-z0-9+/]+={0,2}$/;
const REPOSITORY_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const INSTALLER_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]*\.(?:exe|msi)$/;

const SECRET_PATTERNS = [
  { name: "PEM private key", re: /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----/ },
  { name: "GitHub token", re: /\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/ },
  { name: "AWS access key", re: /\bAKIA[0-9A-Z]{16}\b/ },
  { name: "Slack token", re: /\bxox[baprs]-[A-Za-z0-9-]{10,}\b/ },
  {
    name: "secret environment variable name",
    re: /\b(?:TAURI_SIGNING_PRIVATE_KEY|TAURI_SIGNING_PRIVATE_KEY_PASSWORD|RELEASE_APP_PRIVATE_KEY)\b/,
  },
];

export function parseVersion(value) {
  const text = String(value ?? "")
    .trim()
    .replace(/^v/, "");
  const match = VERSION_RE.exec(text);
  if (!match) {
    throw new Error(`Invalid semantic version: ${JSON.stringify(value)}`);
  }
  return {
    version: text,
    core: [Number(match[1]), Number(match[2]), Number(match[3])],
    prerelease: match[4] ? match[4].split(".") : [],
  };
}

export function parseTag(tag) {
  const text = String(tag ?? "").trim();
  if (!text.startsWith("v")) {
    throw new Error(`Release tag must start with "v": ${JSON.stringify(tag)}`);
  }
  return parseVersion(text);
}

function comparePrerelease(left, right) {
  if (left.length === 0 && right.length === 0) return 0;
  // A release has higher precedence than any prerelease of the same core.
  if (left.length === 0) return 1;
  if (right.length === 0) return -1;

  const count = Math.max(left.length, right.length);
  for (let i = 0; i < count; i += 1) {
    if (i >= left.length) return -1;
    if (i >= right.length) return 1;
    const a = left[i];
    const b = right[i];
    const aNumeric = /^\d+$/.test(a);
    const bNumeric = /^\d+$/.test(b);
    if (aNumeric && bNumeric) {
      const diff = Number(a) - Number(b);
      if (diff !== 0) return diff < 0 ? -1 : 1;
    } else if (aNumeric !== bNumeric) {
      return aNumeric ? -1 : 1;
    } else if (a !== b) {
      return a < b ? -1 : 1;
    }
  }
  return 0;
}

export function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (let i = 0; i < 3; i += 1) {
    if (a.core[i] !== b.core[i]) return a.core[i] < b.core[i] ? -1 : 1;
  }
  return comparePrerelease(a.prerelease, b.prerelease);
}

function validateRepository(repository) {
  const text = String(repository ?? "").trim();
  if (!REPOSITORY_RE.test(text) || text.includes("..")) {
    throw new Error(`Invalid GitHub repository: ${JSON.stringify(repository)}`);
  }
  return text;
}

function validateInstaller(installer, version) {
  const text = String(installer ?? "").trim();
  if (
    !INSTALLER_RE.test(text) ||
    text.includes("..") ||
    !text.includes(`_${version}_`)
  ) {
    throw new Error(
      `Installer name must be a safe .exe/.msi asset for version ${version}: ${JSON.stringify(installer)}`,
    );
  }
  return text;
}

export function buildInstallerUrl(repository, tag, installer) {
  const repo = validateRepository(repository);
  const cleanTag = String(tag ?? "").trim();
  const name = String(installer ?? "").trim();
  const url = `https://github.com/${repo}/releases/download/${cleanTag}/${name}`;
  return validateInstallerUrl(url, repo, cleanTag, name);
}

export function validateInstallerUrl(url, repository, tag, installer) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error("Installer URL is not a valid URL");
  }
  const expectedPath = `/${repository}/releases/download/${tag}/${installer}`;
  if (
    parsed.protocol !== "https:" ||
    parsed.hostname !== "github.com" ||
    parsed.port !== "" ||
    parsed.username !== "" ||
    parsed.password !== "" ||
    parsed.search !== "" ||
    parsed.hash !== "" ||
    parsed.pathname !== expectedPath
  ) {
    throw new Error(
      `Installer URL must be https://github.com/${repository}/releases/download/${tag}/${installer}`,
    );
  }
  return url;
}

export function normalizeSignature(signature) {
  const text = String(signature ?? "").trim();
  if (text.length < 80) {
    throw new Error("Updater signature is too short");
  }
  if (text.length % 4 === 1 || !/^[A-Za-z0-9+/]+={0,2}$/.test(text)) {
    throw new Error("Updater signature is not valid base64");
  }
  const padded = text + "=".repeat((4 - (text.length % 4)) % 4);
  if (padded.length % 4 !== 0 || !BASE64_RE.test(padded)) {
    throw new Error("Updater signature is not valid base64");
  }
  return text;
}

function validateNotes(notes) {
  if (typeof notes !== "string" || notes.trim().length === 0) {
    throw new Error("Release notes must be a non-empty string");
  }
  if (notes.length > 2000) {
    throw new Error("Release notes are too long");
  }
  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F]/.test(notes)) {
    throw new Error("Release notes contain control characters");
  }
  return notes;
}

function validatePubDate(pubDate) {
  const text = String(pubDate ?? "").trim();
  if (!PUB_DATE_RE.test(text) || Number.isNaN(Date.parse(text))) {
    throw new Error("pub_date must be an ISO-8601 UTC timestamp");
  }
  return text;
}

export function buildManifest({
  tag,
  repository,
  installer,
  signature,
  notes,
  pubDate,
}) {
  const parsedTag = parseTag(tag);
  const repo = validateRepository(repository);
  const name = validateInstaller(installer, parsedTag.version);
  const sig = normalizeSignature(signature);
  const url = buildInstallerUrl(repo, tag.trim(), name);
  const date = pubDate
    ? validatePubDate(pubDate)
    : new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
  const releaseNotes =
    notes === undefined ? `Release ${tag.trim()}` : validateNotes(notes);

  return {
    version: parsedTag.version,
    notes: releaseNotes,
    pub_date: date,
    platforms: {
      "windows-x86_64": {
        signature: sig,
        url,
      },
    },
  };
}

export function serializeManifest(manifest) {
  return `${JSON.stringify(manifest, null, 2)}\n`;
}

function assertExactKeys(object, expected, label) {
  if (object === null || typeof object !== "object" || Array.isArray(object)) {
    throw new Error(`${label} must be a JSON object`);
  }
  const actual = Object.keys(object).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, i) => key !== wanted[i])) {
    throw new Error(`${label} has unexpected fields`);
  }
}

export function validateManifest(manifest, expected) {
  assertExactKeys(
    manifest,
    ["version", "notes", "pub_date", "platforms"],
    "latest.json",
  );
  const parsedTag = parseTag(expected.tag);
  if (manifest.version !== parsedTag.version) {
    throw new Error(
      `latest.json version ${JSON.stringify(manifest.version)} does not match tag ${expected.tag}`,
    );
  }
  validateNotes(manifest.notes);
  validatePubDate(manifest.pub_date);
  assertExactKeys(
    manifest.platforms,
    ["windows-x86_64"],
    "latest.json platforms",
  );
  const platform = manifest.platforms["windows-x86_64"];
  assertExactKeys(
    platform,
    ["signature", "url"],
    "latest.json windows-x86_64 platform",
  );

  const repository = validateRepository(expected.repository);
  const installer = validateInstaller(
    expected.installer,
    parsedTag.version,
  );
  normalizeSignature(platform.signature);
  if (expected.signature !== undefined) {
    const expectedSignature = normalizeSignature(expected.signature);
    if (platform.signature !== expectedSignature) {
      throw new Error("latest.json signature does not match the signed installer");
    }
  }
  const expectedUrl = buildInstallerUrl(
    repository,
    expected.tag,
    installer,
  );
  if (platform.url !== expectedUrl) {
    throw new Error("latest.json installer URL does not match the release asset");
  }
  return manifest;
}

export function assertNoSecrets(text, { envNames = [], env = process.env } = {}) {
  const value = String(text ?? "");
  for (const { name, re } of SECRET_PATTERNS) {
    if (re.test(value)) {
      throw new Error(`Updater manifest contains something that looks like a ${name}`);
    }
  }
  for (const name of envNames) {
    const secret = env[name];
    if (typeof secret !== "string" || secret.length < 8) continue;
    const normalized = secret.replace(/\r\n/g, "\n");
    if (value.includes(secret) || value.includes(normalized)) {
      // Never include the secret or its value in the error message.
      throw new Error(`Updater manifest contains the value of ${name}`);
    }
  }
  return value;
}

export function readSignatureFile(path) {
  return normalizeSignature(readFileSync(path, "utf8"));
}

function parseCli(argv) {
  const mode = argv[0];
  if (mode !== "generate" && mode !== "validate") {
    throw new Error("Usage: latest-json.mjs <generate|validate> [options]");
  }
  const options = {};
  for (let i = 1; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    if (key === "dry-run" || key === "quiet") {
      options[key] = true;
      continue;
    }
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }
    options[key] = value;
    i += 1;
  }
  return { mode, options };
}

function requireOption(options, key) {
  const value = options[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Missing required option --${key}`);
  }
  return value;
}

function envNames(options) {
  return (options["forbid-env"] ?? "")
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);
}

function run(argv) {
  const { mode, options } = parseCli(argv);

  if (mode === "generate") {
    const tag = requireOption(options, "tag");
    const repository = requireOption(options, "repository");
    const installer = requireOption(options, "installer");
    const signature = readSignatureFile(
      requireOption(options, "signature-file"),
    );
    const manifest = buildManifest({
      tag,
      repository,
      installer,
      signature,
      notes: options.notes,
      pubDate: options["pub-date"],
    });
    const text = serializeManifest(manifest);
    assertNoSecrets(text, { envNames: envNames(options) });

    if (options["dry-run"]) {
      process.stdout.write(text);
      return;
    }
    const output = requireOption(options, "output");
    writeFileSync(output, text);
    if (!options.quiet) {
      console.log(`Generated ${output} for ${tag} (version ${manifest.version})`);
    }
    return;
  }

  const file = requireOption(options, "file");
  const tag = requireOption(options, "tag");
  const repository = requireOption(options, "repository");
  const installer = requireOption(options, "installer");
  const signature = options["signature-file"]
    ? readSignatureFile(options["signature-file"])
    : undefined;
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    throw new Error(`latest.json is not valid JSON: ${error.message}`);
  }
  validateManifest(manifest, {
    tag,
    repository,
    installer,
    signature,
  });
  assertNoSecrets(readFileSync(file, "utf8"), {
    envNames: envNames(options),
  });
  if (!options.quiet) {
    console.log(`latest.json is valid for ${tag}`);
  }
}

const isMain =
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
