import { test } from "node:test";
import assert from "node:assert/strict";

import { buildManifest, serializeManifest } from "./latest-json.mjs";
import { publishLatestJson } from "./publish-latest-json.mjs";

const REPO = "veb898-cyber/VoidLauncher";
const TAG = "v1.6.0";
const BRANCH = "release/latest-json-v1.6.0";
const TOKEN = "test-installation-token-value";

const OLD_SIGNATURE = Buffer.from("old installer signature ".repeat(8)).toString(
  "base64",
);
const NEW_SIGNATURE = Buffer.from("new installer signature ".repeat(8)).toString(
  "base64",
);

function manifestFor(version, signature) {
  return buildManifest({
    tag: `v${version}`,
    repository: REPO,
    installer: `VoidLauncher_${version}_x64-setup.exe`,
    signature,
    notes: `Release v${version}`,
    pubDate: "2026-11-15T12:00:00Z",
  });
}

const OLD_MANIFEST = serializeManifest(manifestFor("1.5.0", OLD_SIGNATURE));
const NEW_MANIFEST = serializeManifest(manifestFor("1.6.0", NEW_SIGNATURE));
const NEW_MANIFEST_OTHER_SIGNATURE = serializeManifest(
  manifestFor("1.6.0", OLD_SIGNATURE),
);
const MAIN_MANIFEST = serializeManifest(manifestFor("1.7.0", NEW_SIGNATURE));

function response(status, body) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

class FakeGitHub {
  constructor({ mainManifest = OLD_MANIFEST, branchManifest = null, pr = null } = {}) {
    this.repo = REPO;
    this.refs = new Map([["main", "main-sha"]]);
    this.files = new Map([["main", { text: mainManifest, sha: "main-file-sha" }]]);
    this.pulls = [];
    this.mutations = [];
    this.requests = [];
    this.compareFilesList = [{ filename: "latest.json" }];
    this.nextCommit = 1;
    this.nextPr = 42;
    if (branchManifest !== null) {
      this.refs.set(BRANCH, "branch-sha");
      this.files.set(BRANCH, { text: branchManifest, sha: "branch-file-sha" });
    }
    if (pr) {
      this.pulls.push({
        number: this.nextPr,
        state: "open",
        title: `chore(release): update latest.json for ${TAG}`,
        body: "",
        html_url: `https://github.com/${REPO}/pull/${this.nextPr}`,
        head: { ref: BRANCH, sha: this.refs.get(BRANCH) ?? "branch-sha" },
        mergeable_state: "clean",
        files: [{ filename: "latest.json" }],
        ...pr,
      });
      this.nextPr += 1;
    }
  }

  mutation(method, path, body) {
    this.mutations.push({ method, path, body });
  }

  syncPull(pull) {
    pull.head = {
      ...pull.head,
      sha: this.refs.get(pull.head?.ref ?? BRANCH) ?? pull.head?.sha,
    };
    return pull;
  }

  async fetch(url, init = {}) {
    const parsed = new URL(url);
    const method = (init.method ?? "GET").toUpperCase();
    const path = parsed.pathname;
    const body = init.body ? JSON.parse(init.body) : undefined;
    this.requests.push({ method, url, body });

    if (method !== "GET") {
      this.mutation(method, path, body);
    }

    const prefix = `/repos/${REPO}`;
    if (!path.startsWith(prefix)) {
      return response(404, { message: "not found" });
    }
    const rest = path.slice(prefix.length);

    if (method === "GET" && rest === "/git/ref/heads/main") {
      return response(200, { object: { sha: this.refs.get("main") } });
    }
    if (method === "GET" && rest.startsWith("/git/ref/heads/")) {
      const branch = decodeURIComponent(rest.slice("/git/ref/heads/".length));
      const sha = this.refs.get(branch);
      if (!sha) return response(404, { message: "not found" });
      return response(200, { object: { sha } });
    }
    if (method === "POST" && rest === "/git/refs") {
      const branch = body.ref.slice("refs/heads/".length);
      if (this.refs.has(branch)) {
        return response(422, { message: "Reference already exists" });
      }
      this.refs.set(branch, "branch-sha");
      return response(201, { ref: body.ref, object: { sha: "branch-sha" } });
    }
    if (method === "GET" && rest === "/contents/latest.json") {
      const ref = parsed.searchParams.get("ref");
      const file = this.files.get(ref);
      if (!file) return response(404, { message: "not found" });
      return response(200, {
        sha: file.sha,
        encoding: "base64",
        content: Buffer.from(file.text, "utf8").toString("base64"),
      });
    }
    if (method === "PUT" && rest === "/contents/latest.json") {
      const file = this.files.get(body.branch);
      if (file && body.sha !== file.sha) {
        return response(409, { message: "sha does not match" });
      }
      if (!file && body.sha) {
        return response(409, { message: "sha does not match" });
      }
      const text = Buffer.from(body.content, "base64").toString("utf8");
      this.files.set(body.branch, {
        text,
        sha: `commit-${this.nextCommit}`,
      });
      this.nextCommit += 1;
      return response(200, { commit: { sha: "commit-sha" } });
    }
    if (method === "GET" && rest === "/pulls") {
      const head = decodeURIComponent(parsed.searchParams.get("head") ?? "");
      const wants = head.split(":")[1];
      const open = this.pulls
        .filter((pull) => pull.state === "open" && pull.head.ref === wants)
        .map((pull) => this.syncPull(pull));
      return response(200, open);
    }
    if (method === "POST" && rest === "/pulls") {
      if (
        this.pulls.some(
          (pull) => pull.state === "open" && pull.head.ref === body.head,
        )
      ) {
        return response(422, {
          message: "A pull request already exists for this branch",
        });
      }
      const pull = {
        number: this.nextPr,
        state: "open",
        title: body.title,
        body: body.body,
        html_url: `https://github.com/${REPO}/pull/${this.nextPr}`,
        head: { ref: body.head, sha: this.refs.get(body.head) },
        mergeable_state: "clean",
        files: [{ filename: "latest.json" }],
      };
      this.nextPr += 1;
      this.pulls.push(pull);
      return response(201, this.syncPull(pull));
    }
    const pullMatch = rest.match(/^\/pulls\/(\d+)(\/files|\/update-branch)?$/);
    if (pullMatch) {
      const pull = this.pulls.find((item) => item.number === Number(pullMatch[1]));
      if (!pull) return response(404, { message: "not found" });
      if (method === "GET" && pullMatch[2] === "/files") {
        return response(200, pull.files ?? [{ filename: "latest.json" }]);
      }
      if (method === "GET" && !pullMatch[2]) {
        return response(200, this.syncPull(pull));
      }
      if (method === "PATCH" && !pullMatch[2]) {
        pull.title = body.title;
        pull.body = body.body;
        return response(200, this.syncPull(pull));
      }
      if (method === "PUT" && pullMatch[2] === "/update-branch") {
        if (body.expected_head_sha !== this.refs.get(pull.head.ref)) {
          return response(409, { message: "head sha changed" });
        }
        this.refs.set(pull.head.ref, "merged-branch-sha");
        pull.mergeable_state = "clean";
        return response(202, { message: "Updating pull request branch." });
      }
    }
    const compareMatch = rest.match(/^\/compare\/([^/]+)\.\.\.([^/]+)$/);
    if (method === "GET" && compareMatch) {
      return response(200, { files: this.compareFilesList });
    }
    return response(404, { message: `unhandled ${method} ${rest}` });
  }
}

function options(fake, overrides = {}) {
  return {
    repo: REPO,
    tag: TAG,
    manifestText: NEW_MANIFEST,
    token: TOKEN,
    fetchImpl: fake.fetch.bind(fake),
    log: () => {},
    ...overrides,
  };
}

function mutationPaths(fake) {
  return fake.mutations.map((item) => `${item.method} ${item.path}`);
}

test("creates a release branch, commits the manifest, and opens a PR", async () => {
  const fake = new FakeGitHub();
  const result = await publishLatestJson(options(fake));

  assert.equal(result.action, "create-pr");
  assert.equal(result.prNumber, 42);
  assert.match(result.prUrl, /\/pull\/42$/);
  assert.ok(
    mutationPaths(fake).includes("POST /repos/veb898-cyber/VoidLauncher/git/refs"),
  );
  assert.ok(
    mutationPaths(fake).includes(
      "PUT /repos/veb898-cyber/VoidLauncher/contents/latest.json",
    ),
  );
  assert.ok(
    mutationPaths(fake).includes("POST /repos/veb898-cyber/VoidLauncher/pulls"),
  );
  assert.ok(
    fake.mutations.every(
      (item) =>
        item.path !== "/repos/veb898-cyber/VoidLauncher/contents/latest.json" ||
        item.body.branch !== "main",
    ),
    "latest.json must never be written on main",
  );
});

test("rerun is idempotent and does not create a second PR or commit", async () => {
  const fake = new FakeGitHub();
  await publishLatestJson(options(fake));
  const mutationsAfterFirstRun = fake.mutations.length;

  const result = await publishLatestJson(options(fake));

  assert.equal(result.action, "unchanged-pr");
  assert.equal(result.prNumber, 42);
  assert.equal(fake.mutations.length, mutationsAfterFirstRun);
  assert.equal(fake.pulls.length, 1);
});

test("updates an existing PR branch in place without a duplicate PR", async () => {
  const fake = new FakeGitHub({
    branchManifest: NEW_MANIFEST_OTHER_SIGNATURE,
    pr: { body: "old body" },
  });

  const result = await publishLatestJson(options(fake));

  assert.equal(result.action, "updated-pr");
  assert.equal(result.committed, true);
  assert.ok(
    mutationPaths(fake).some((path) =>
      path.startsWith("PUT /repos/veb898-cyber/VoidLauncher/contents/"),
    ),
  );
  assert.ok(
    mutationPaths(fake).some((path) =>
      path.startsWith("PATCH /repos/veb898-cyber/VoidLauncher/pulls/42"),
    ),
  );
  assert.ok(
    !mutationPaths(fake).includes("POST /repos/veb898-cyber/VoidLauncher/pulls"),
  );
  assert.equal(fake.pulls.length, 1);
});

test("skips everything when main already has the same version and signature", async () => {
  const fake = new FakeGitHub({ mainManifest: NEW_MANIFEST });
  const result = await publishLatestJson(options(fake));

  assert.equal(result.action, "skip");
  assert.equal(result.reason, "main-already-current");
  assert.deepEqual(fake.mutations, []);
});

test("refuses to downgrade a newer manifest on main", async () => {
  const fake = new FakeGitHub({ mainManifest: MAIN_MANIFEST });
  await assert.rejects(
    publishLatestJson(options(fake)),
    /newer version 1\.7\.0.*refusing to downgrade/,
  );
  assert.deepEqual(fake.mutations, []);
});

test("refuses a release PR that contains unrelated files", async () => {
  const fake = new FakeGitHub({
    branchManifest: NEW_MANIFEST_OTHER_SIGNATURE,
    pr: {
      files: [
        { filename: "latest.json" },
        { filename: "src-tauri/src/main.rs" },
      ],
    },
  });

  await assert.rejects(
    publishLatestJson(options(fake)),
    /files other than latest\.json/,
  );
  assert.deepEqual(fake.mutations, []);
});

test("refuses a pre-existing branch with unrelated changes", async () => {
  const fake = new FakeGitHub({
    branchManifest: NEW_MANIFEST_OTHER_SIGNATURE,
  });
  fake.compareFilesList = [
    { filename: "latest.json" },
    { filename: "src-tauri/src/main.rs" },
  ];

  await assert.rejects(
    publishLatestJson(options(fake)),
    /files other than latest\.json/,
  );
  assert.deepEqual(fake.mutations, []);
});

test("opens a PR for an existing manifest-only branch without a new commit", async () => {
  const fake = new FakeGitHub({ branchManifest: NEW_MANIFEST });
  const result = await publishLatestJson(options(fake));

  assert.equal(result.action, "create-pr");
  assert.equal(result.committed, false);
  assert.equal(
    mutationPaths(fake).filter((path) =>
      path.startsWith("PUT /repos/veb898-cyber/VoidLauncher/contents/"),
    ).length,
    0,
  );
  assert.ok(
    mutationPaths(fake).includes("POST /repos/veb898-cyber/VoidLauncher/pulls"),
  );
});

test("refuses a release PR with a merge conflict", async () => {
  const fake = new FakeGitHub({
    branchManifest: NEW_MANIFEST_OTHER_SIGNATURE,
    pr: { mergeable_state: "dirty" },
  });
  await assert.rejects(
    publishLatestJson(options(fake)),
    /merge conflict with main/,
  );
  assert.deepEqual(fake.mutations, []);
});

test("dry run performs no mutations", async () => {
  const fake = new FakeGitHub();
  const logs = [];
  const result = await publishLatestJson(
    options(fake, { dryRun: true, log: (line) => logs.push(line) }),
  );

  assert.equal(result.action, "create-pr");
  assert.equal(result.dryRun, true);
  assert.deepEqual(fake.mutations, []);
  assert.ok(logs.some((line) => line.includes("Dry run")));
});

test("updates a behind PR branch by merging main without force", async () => {
  const fake = new FakeGitHub({
    branchManifest: NEW_MANIFEST,
    pr: { mergeable_state: "behind" },
  });

  const result = await publishLatestJson(options(fake));

  assert.equal(result.updatedBranch, true);
  assert.ok(
    mutationPaths(fake).includes(
      "PUT /repos/veb898-cyber/VoidLauncher/pulls/42/update-branch",
    ),
  );
  assert.ok(
    fake.mutations.every(
      (item) => item.method !== "POST" || item.path !== "/graphql",
    ),
    "no force-push or GraphQL rewrite is used",
  );
});

test("requires an installation token and never logs it", async () => {
  const fake = new FakeGitHub();
  await assert.rejects(
    publishLatestJson(options(fake, { token: "" })),
    /GH_TOKEN is not set/,
  );
  assert.deepEqual(fake.requests, []);

  const logs = [];
  await publishLatestJson(options(fake, { log: (line) => logs.push(line) }));
  for (const line of logs) {
    assert.ok(!line.includes(TOKEN));
  }
});

test("only talks to the GitHub HTTPS API", async () => {
  const fake = new FakeGitHub();
  await publishLatestJson(options(fake));
  assert.ok(fake.requests.length > 0);
  for (const request of fake.requests) {
    assert.ok(request.url.startsWith("https://api.github.com/"), request.url);
  }
});
