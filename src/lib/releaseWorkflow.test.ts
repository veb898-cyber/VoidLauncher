import { describe, expect, it } from 'vitest';
import workflow from '../../.github/workflows/publish.yml?raw';

/**
 * Static regression checks for the release workflow. These do not run the
 * release (that requires secrets and a tag), but they lock in the properties
 * that previously made publishes fail or leak:
 *   - `latest.json` must be committed on top of the CURRENT main, not the
 *     detached tag commit (a non-fast-forward push otherwise);
 *   - the tag must match every version file before anything is built;
 *   - the signing key must never be echoed (no `set -x`).
 */
describe('release workflow', () => {
  it('updates latest.json on the current main with retries', () => {
    expect(workflow).toContain('git fetch origin main');
    expect(workflow).toContain('git checkout -B latest-json-update FETCH_HEAD');
    expect(workflow).toContain('git push origin HEAD:main');
    expect(workflow).toContain('attempt=$((attempt + 1))');
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
