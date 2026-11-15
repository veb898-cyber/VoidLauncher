import { describe, expect, it } from 'vitest';
import workflow from '../../.github/workflows/publish.yml?raw';

/**
 * Static regression checks for the release workflow. These do not run the
 * release (that requires secrets and a tag), but they lock in the properties
 * that previously made publishes fail or leak:
 *   - `latest.json` must be published through a PR, never pushed to `main`;
 *   - the PR branch/PR must be created with a minimally scoped App token;
 *   - the token may only be requested after the installer was signed;
 *   - the tag must match every version file before anything is built;
 *   - the signing key must never be echoed (no `set -x`).
 */
describe('release workflow', () => {
  it('publishes latest.json through a pull request instead of pushing to main', () => {
    expect(workflow).not.toContain('git push');
    expect(workflow).not.toContain('HEAD:main');
    expect(workflow).toContain('release/latest-json-v');
    expect(workflow).toContain('publish-latest-json.mjs');
  });

  it('creates a minimally scoped GitHub App token after signing', () => {
    expect(workflow).toContain('actions/create-github-app-token@v3');
    expect(workflow).toContain('secrets.RELEASE_APP_ID');
    expect(workflow).toContain('secrets.RELEASE_APP_PRIVATE_KEY');
    expect(workflow).toContain('permission-contents: write');
    expect(workflow).toContain('permission-pull-requests: write');
    expect(workflow.indexOf('Create GitHub App token')).toBeGreaterThan(
      workflow.indexOf('Sign installer and generate latest.json'),
    );
    expect(workflow.indexOf('Publish latest.json via pull request')).toBeGreaterThan(
      workflow.indexOf('Create GitHub App token'),
    );
  });

  it('verifies that the tag matches every project version', () => {
    expect(workflow).toContain("require('./package.json').version");
    expect(workflow).toContain("require('./src-tauri/tauri.conf.json').version");
    expect(workflow).toContain('src-tauri/Cargo.toml');
    expect(workflow).toContain('does not match the project version');
  });

  it('never enables shell tracing or echoes the signing key', () => {
    expect(workflow).not.toContain('set -x');
    expect(workflow).not.toContain('echo "$TAURI_SIGNING_PRIVATE_KEY"');
    expect(workflow).toContain('Do NOT use -x here');
  });
});
