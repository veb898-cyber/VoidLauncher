#!/usr/bin/env node
// Publish the updater manifest through a protected-branch pull request.
//
// The script never runs `git push` and never writes to `main`. It uses the
// GitHub REST API with a GitHub App installation token (supplied through
// GH_TOKEN) to:
//   1. create/update the dedicated `release/latest-json-vX.Y.Z` branch;
//   2. create or update the release pull request;
//   3. keep reruns idempotent (no duplicate PRs, no empty commits, no force).
//
// `--dry-run` performs only GET requests and prints the planned mutations.

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import {
  assertNoSecrets,
  compareVersions,
  parseTag,
  validateManifest,
} from "./latest-json.mjs";

const API_VERSION = "2022-11-28";
const DEFAULT_API_URL = "https://api.github.com";
const MANIFEST_PATH = "latest.json";

function parseJson(text, label) {
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`${label} is not valid JSON; refusing to overwrite it`);
  }
}

function platformOf(manifest) {
  return manifest?.platforms?.["windows-x86_64"];
}

function manifestIdentity(manifest) {
  const platform = platformOf(manifest);
  return JSON.stringify({
    version: manifest.version,
    signature: platform?.signature,
    url: platform?.url,
  });
}

function readDesiredManifest(text, expectedTag) {
  const manifest = parseJson(text, "generated latest.json");
  const parsedTag = parseTag(expectedTag);
  if (manifest.version !== parsedTag.version) {
    throw new Error(
      `Generated latest.json version ${JSON.stringify(manifest.version)} does not match tag ${expectedTag}`,
    );
  }
  const platform = platformOf(manifest);
  if (
    typeof platform?.signature !== "string" ||
    platform.signature.length === 0 ||
    typeof platform?.url !== "string" ||
    platform.url.length === 0
  ) {
    throw new Error("Generated latest.json is missing the windows-x86_64 signature/url");
  }
  return manifest;
}

function createClient({ repo, token, fetchImpl, apiUrl }) {
  const owner = repo.split("/")[0];

  async function request(path, { method = "GET", body } = {}) {
    const response = await fetchImpl(`${apiUrl}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": API_VERSION,
        "User-Agent": "VoidLauncher-release-workflow",
        ...(body ? { "Content-Type": "application/json" } : {}),
      },
      body: body ? JSON.stringify(body) : undefined,
    });

    const text = await response.text();
    if (response.status === 404) return null;
    if (!response.ok) {
      const error = new Error(
        `GitHub API ${method} ${path} failed with ${response.status}: ${text.slice(0, 500)}`,
      );
      error.status = response.status;
      error.body = text;
      throw error;
    }
    if (!text) return {};
    try {
      return JSON.parse(text);
    } catch {
      return text;
    }
  }

  async function getRef(name) {
    return request(`/repos/${repo}/git/ref/heads/${name}`);
  }

  async function getFile(ref) {
    const file = await request(
      `/repos/${repo}/contents/${MANIFEST_PATH}?ref=${encodeURIComponent(ref)}`,
    );
    if (!file) return null;
    if (file.encoding !== "base64" || typeof file.content !== "string") {
      throw new Error(`GitHub returned an unsupported representation for ${MANIFEST_PATH}`);
    }
    return {
      sha: file.sha,
      text: Buffer.from(file.content.replace(/\s/g, ""), "base64").toString(
        "utf8",
      ),
    };
  }

  async function createRef(branch, sha) {
    try {
      return await request(`/repos/${repo}/git/refs`, {
        method: "POST",
        body: { ref: `refs/heads/${branch}`, sha },
      });
    } catch (error) {
      // Another run may have created the branch between our GET and POST.
      if (error.status === 422) return getRef(branch);
      throw error;
    }
  }

  async function putFile({ branch, text, sha, message }) {
    return request(`/repos/${repo}/contents/${MANIFEST_PATH}`, {
      method: "PUT",
      body: {
        message,
        content: Buffer.from(text, "utf8").toString("base64"),
        branch,
        ...(sha ? { sha } : {}),
      },
    });
  }

  async function listOpenPulls(branch) {
    const pulls = await request(
      `/repos/${repo}/pulls?head=${encodeURIComponent(`${owner}:${branch}`)}&state=open&per_page=10`,
    );
    return Array.isArray(pulls) ? pulls : [];
  }

  async function getPull(number) {
    return request(`/repos/${repo}/pulls/${number}`);
  }

  async function getPullFiles(number) {
    const files = await request(
      `/repos/${repo}/pulls/${number}/files?per_page=100`,
    );
    return Array.isArray(files) ? files : [];
  }

  async function createPull({ title, head, body }) {
    return request(`/repos/${repo}/pulls`, {
      method: "POST",
      body: { title, head, base: "main", body },
    });
  }

  async function updatePull(number, { title, body }) {
    return request(`/repos/${repo}/pulls/${number}`, {
      method: "PATCH",
      body: { title, body },
    });
  }

  async function updatePullBranch(number, expectedHeadSha) {
    return request(`/repos/${repo}/pulls/${number}/update-branch`, {
      method: "PUT",
      body: { expected_head_sha: expectedHeadSha },
    });
  }

  async function compareFiles(baseSha, headSha) {
    const comparison = await request(
      `/repos/${repo}/compare/${baseSha}...${headSha}`,
    );
    return Array.isArray(comparison?.files) ? comparison.files : [];
  }

  return {
    getRef,
    getFile,
    createRef,
    putFile,
    listOpenPulls,
    getPull,
    getPullFiles,
    createPull,
    updatePull,
    updatePullBranch,
    compareFiles,
  };
}

function assertOnlyManifest(files, context) {
  if (files.length >= 100) {
    throw new Error(
      `${context} contains too many changed files to verify safely. ` +
        "Refusing to continue.",
    );
  }
  const foreign = files
    .map((file) => file.filename)
    .filter((filename) => filename !== MANIFEST_PATH);
  if (foreign.length > 0) {
    throw new Error(
      `${context} contains files other than ${MANIFEST_PATH}: ${foreign.join(", ")}. ` +
        "Refusing to include unrelated changes in the release manifest PR.",
    );
  }
}

function buildPullBody({ tag, manifest }) {
  return [
    `Automated updater manifest update for \`${tag}\`.`,
    "",
    `- Version: \`${manifest.version}\``,
    `- Platform: \`windows-x86_64\``,
    `- Installer: ${platformOf(manifest).url}`,
    "",
    "This PR changes only `latest.json`. It is created by the Release workflow " +
      "after the installer was built and signed; the required checks must pass " +
      "before merge. Reruns update this PR in place and never force-push.",
    "",
  ].join("\n");
}

function buildPullTitle(tag) {
  return `chore(release): update latest.json for ${tag}`;
}

async function writeManifestIfNeeded({
  client,
  branch,
  desiredText,
  desiredIdentity,
  message,
  dryRun,
}) {
  const current = await client.getFile(branch);
  if (current) {
    const currentManifest = parseJson(
      current.text,
      `latest.json on ${branch}`,
    );
    if (manifestIdentity(currentManifest) === desiredIdentity) {
      return false;
    }
  }
  if (dryRun) return true;

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    const latest = await client.getFile(branch);
    if (latest) {
      const latestManifest = parseJson(
        latest.text,
        `latest.json on ${branch}`,
      );
      if (manifestIdentity(latestManifest) === desiredIdentity) return false;
    }
    try {
      await client.putFile({
        branch,
        text: desiredText,
        sha: latest?.sha,
        message,
      });
      return true;
    } catch (error) {
      if (error.status === 409 || error.status === 422) {
        // The branch moved between GET and PUT. Re-read and retry without
        // force-updating anything.
        continue;
      }
      throw error;
    }
  }
  throw new Error(
    `Could not update ${MANIFEST_PATH} on ${branch} after 3 attempts`,
  );
}

async function findOpenPull(client, branch) {
  const pulls = await client.listOpenPulls(branch);
  return pulls.length > 0 ? pulls[0] : null;
}

export async function publishLatestJson({
  repo,
  tag,
  manifestText,
  token,
  branch,
  title,
  body,
  dryRun = false,
  fetchImpl = globalThis.fetch,
  apiUrl = DEFAULT_API_URL,
  log = console.log,
}) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(String(repo ?? ""))) {
    throw new Error(`Invalid repository: ${JSON.stringify(repo)}`);
  }
  if (typeof token !== "string" || token.length === 0) {
    throw new Error("GH_TOKEN is not set; a GitHub App installation token is required");
  }
  assertNoSecrets(manifestText, {
    envNames: [
      "GH_TOKEN",
      "RELEASE_APP_PRIVATE_KEY",
      "TAURI_SIGNING_PRIVATE_KEY",
      "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
    ],
  });
  const desired = readDesiredManifest(manifestText, tag);
  const parsedTag = parseTag(tag);
  const releaseBranch =
    branch ?? `release/latest-json-v${parsedTag.version}`;
  if (!/^release\/latest-json-v[0-9A-Za-z.+-]+$/.test(releaseBranch)) {
    throw new Error(`Invalid release branch name: ${JSON.stringify(releaseBranch)}`);
  }
  const pullTitle = title ?? buildPullTitle(tag);
  const pullBody = body ?? buildPullBody({ tag, manifest: desired });
  const desiredIdentity = manifestIdentity(desired);
  const client = createClient({ repo, token, fetchImpl, apiUrl });

  const mainRef = await client.getRef("main");
  if (!mainRef?.object?.sha) {
    throw new Error("Could not resolve the current main branch");
  }
  const mainSha = mainRef.object.sha;
  const mainFile = await client.getFile("main");

  if (mainFile) {
    const mainManifest = parseJson(mainFile.text, "latest.json on main");
    let comparison;
    try {
      comparison = compareVersions(mainManifest.version, desired.version);
    } catch {
      throw new Error(
        "latest.json on main has an invalid version; refusing to overwrite it",
      );
    }
    if (comparison > 0) {
      throw new Error(
        `main already publishes newer version ${mainManifest.version}; ` +
          `refusing to downgrade it to ${desired.version}`,
      );
    }
    if (comparison === 0 && manifestIdentity(mainManifest) === desiredIdentity) {
      log(`latest.json on main already matches ${tag}; no PR is needed.`);
      const existing = await findOpenPull(client, releaseBranch);
      if (existing) {
        log(`Existing release PR left unchanged: ${existing.html_url}`);
      }
      return {
        action: "skip",
        reason: "main-already-current",
        branch: releaseBranch,
        prNumber: existing?.number ?? null,
        prUrl: existing?.html_url ?? null,
      };
    }
  }

  const openPull = await findOpenPull(client, releaseBranch);
  const branchRef = await client.getRef(releaseBranch);

  if (openPull) {
    let pull = (await client.getPull(openPull.number)) ?? openPull;
    if (pull.mergeable_state === "dirty") {
      throw new Error(
        `Release PR #${openPull.number} has a merge conflict with main; ` +
          "resolve it manually. The workflow will not force-push.",
      );
    }
    assertOnlyManifest(
      await client.getPullFiles(openPull.number),
      `Release PR #${openPull.number}`,
    );
    if (!branchRef?.object?.sha) {
      throw new Error(
        `PR #${openPull.number} exists but branch ${releaseBranch} is missing`,
      );
    }
    const committed = await writeManifestIfNeeded({
      client,
      branch: releaseBranch,
      desiredText: manifestText,
      desiredIdentity,
      message: `chore(release): update latest.json for ${tag}`,
      dryRun,
    });

    if (!dryRun && committed) {
      pull = await client.getPull(openPull.number);
    }
    let updatedBranch = false;
    if (pull?.mergeable_state === "dirty") {
      throw new Error(
        `Release PR #${openPull.number} has a merge conflict with main; ` +
          "resolve it manually. The workflow will not force-push.",
      );
    }
    if (
      !dryRun &&
      pull?.mergeable_state === "behind" &&
      pull?.head?.sha
    ) {
      try {
        await client.updatePullBranch(openPull.number, pull.head.sha);
        updatedBranch = true;
      } catch (error) {
        throw new Error(
          `Release PR #${openPull.number} is behind main and could not be updated ` +
            `without forcing (${error.message}). Resolve it manually.`,
        );
      }
    }
    const needsMetadataUpdate =
      pull?.title !== pullTitle || pull?.body !== pullBody;
    if (!dryRun && needsMetadataUpdate) {
      pull = await client.updatePull(openPull.number, {
        title: pullTitle,
        body: pullBody,
      });
    }
    const prUrl = pull?.html_url ?? openPull.html_url;
    if (dryRun) {
      log(
        `Dry run: would ${committed ? "update" : "keep"} ${releaseBranch}, ` +
          `${needsMetadataUpdate ? "refresh" : "keep"} PR #${openPull.number}.`,
      );
    } else if (committed || updatedBranch || needsMetadataUpdate) {
      log(`Updated release PR #${openPull.number}: ${prUrl}`);
    } else {
      log(`Release PR #${openPull.number} is already up to date: ${prUrl}`);
    }
    return {
      action: committed || updatedBranch ? "updated-pr" : "unchanged-pr",
      branch: releaseBranch,
      prNumber: openPull.number,
      prUrl,
      committed,
      updatedBranch,
      updatedMetadata: needsMetadataUpdate,
      dryRun,
    };
  }

  if (branchRef?.object?.sha) {
    assertOnlyManifest(
      await client.compareFiles(mainSha, branchRef.object.sha),
      `Branch ${releaseBranch}`,
    );
  }

  let createdBranch = false;
  if (!branchRef?.object?.sha) {
    if (!dryRun) {
      await client.createRef(releaseBranch, mainSha);
    }
    createdBranch = true;
  }

  const committed = await writeManifestIfNeeded({
    client,
    branch: releaseBranch,
    desiredText: manifestText,
    desiredIdentity,
    message: `chore(release): update latest.json for ${tag}`,
    dryRun,
  });

  if (dryRun) {
    log(
      `Dry run: would create ${releaseBranch} from main, ` +
        `commit ${MANIFEST_PATH}, and open a PR to main.`,
    );
    return {
      action: "create-pr",
      branch: releaseBranch,
      prNumber: null,
      prUrl: null,
      createdBranch,
      committed,
      dryRun,
    };
  }

  let pull;
  try {
    pull = await client.createPull({
      title: pullTitle,
      head: releaseBranch,
      body: pullBody,
    });
  } catch (error) {
    if (
      error.status === 422 &&
      /already exists/i.test(String(error.body ?? ""))
    ) {
      const raced = await findOpenPull(client, releaseBranch);
      if (raced) {
        pull = await client.updatePull(raced.number, {
          title: pullTitle,
          body: pullBody,
        });
      } else {
        throw error;
      }
    } else {
      throw error;
    }
  }

  log(`Opened release PR #${pull.number}: ${pull.html_url}`);
  return {
    action: "create-pr",
    branch: releaseBranch,
    prNumber: pull.number,
    prUrl: pull.html_url,
    createdBranch,
    committed,
    dryRun: false,
  };
}

function parseCli(argv) {
  const options = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    if (key === "dry-run") {
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
  return options;
}

function requireOption(options, key) {
  const value = options[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Missing required option --${key}`);
  }
  return value;
}

async function runCli(argv) {
  const options = parseCli(argv);
  const repo = requireOption(options, "repo");
  const tag = requireOption(options, "tag");
  const manifestPath = requireOption(options, "manifest");
  const tokenEnv = options["token-env"] ?? "GH_TOKEN";
  const token = process.env[tokenEnv];
  const manifestText = readFileSync(manifestPath, "utf8");
  const manifest = parseJson(manifestText, "generated latest.json");
  const installer =
    options.installer ??
    manifest.platforms?.["windows-x86_64"]?.url?.split("/").pop();
  validateManifest(manifest, {
    tag,
    repository: repo,
    installer,
  });
  assertNoSecrets(manifestText, {
    envNames: (options["forbid-env"] ?? "")
      .split(",")
      .map((name) => name.trim())
      .filter(Boolean),
  });
  let body;
  if (options["body-file"]) {
    body = readFileSync(options["body-file"], "utf8");
  } else if (options.body !== undefined) {
    body = options.body;
  }

  await publishLatestJson({
    repo,
    tag,
    manifestText,
    token,
    branch: options.branch,
    title: options.title,
    body,
    dryRun: Boolean(options["dry-run"]),
  });
}

const isMain =
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  runCli(process.argv.slice(2)).catch((error) => {
    console.error(`::error::${error.message}`);
    process.exitCode = 1;
  });
}
