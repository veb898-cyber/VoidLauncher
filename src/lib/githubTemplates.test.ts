import { describe, expect, it } from 'vitest';
import bugForm from '../../.github/ISSUE_TEMPLATE/bug_report.yml?raw';
import featureForm from '../../.github/ISSUE_TEMPLATE/feature_request.yml?raw';
import questionForm from '../../.github/ISSUE_TEMPLATE/question.yml?raw';
import issueConfig from '../../.github/ISSUE_TEMPLATE/config.yml?raw';
import pullRequestTemplate from '../../.github/pull_request_template.md?raw';
import contributing from '../../CONTRIBUTING.md?raw';
import security from '../../SECURITY.md?raw';
import readme from '../../README.md?raw';
import { BUG_REPORT_FORM_FIELDS } from './bugReport';

function fieldBlock(yaml: string, id: string): string {
  const lines = yaml.split('\n');
  const start = lines.findIndex((line) => new RegExp(`^\\s*id:\\s*${id}\\s*$`).test(line));
  if (start < 0) return '';
  let end = start + 1;
  while (end < lines.length && !/^\s{2}-\s+type:/.test(lines[end])) end += 1;
  return lines.slice(start, end).join('\n');
}

function expectField(yaml: string, id: string) {
  expect(yaml, `missing issue-form field id: ${id}`).toMatch(
    new RegExp(`^\\s*id:\\s*${id}\\s*$`, 'm'),
  );
}

describe('GitHub issue forms', () => {
  it('declares every field the launcher prefills', () => {
    for (const id of BUG_REPORT_FORM_FIELDS) expectField(bugForm, id);
  });

  it('requires the core bug-report fields and the privacy checklist', () => {
    for (const id of ['summary', 'steps', 'expected', 'actual', 'checklist']) {
      expect(fieldBlock(bugForm, id), id).toContain('required: true');
    }
    expect(bugForm).toContain('I removed tokens, passwords, session IDs');
    expect(bugForm).toContain('labels:');
    expect(bugForm).toContain('- bug');
  });

  it('has a structured feature request form', () => {
    for (const id of ['problem', 'proposal', 'checklist']) {
      expectField(featureForm, id);
      expect(fieldBlock(featureForm, id), id).toContain('required: true');
    }
    expect(featureForm).toContain('enhancement');
  });

  it('has a question form for users who need help', () => {
    for (const id of ['question', 'checklist']) {
      expectField(questionForm, id);
      expect(fieldBlock(questionForm, id), id).toContain('required: true');
    }
    expect(questionForm).toContain('question');
  });

  it('routes security reports away from public issues', () => {
    expect(issueConfig).toContain('blank_issues_enabled: false');
    expect(issueConfig).toContain('security/policy');
    expect(issueConfig).toContain('security');
  });
});

describe('contributor documentation', () => {
  it('uses the real development and test commands', () => {
    for (const command of [
      'npm install',
      'npm run tauri dev',
      'npm run build',
      'npx tsc --noEmit',
      'npm test',
      'cargo fmt --all -- --check',
      'cargo clippy --all-targets -- -D warnings',
      'cargo test',
    ]) {
      expect(contributing, command).toContain(command);
    }
  });

  it('documents the pull-request workflow and testing expectations', () => {
    expect(pullRequestTemplate).toContain('## Summary');
    expect(pullRequestTemplate).toContain('## How tested');
    expect(pullRequestTemplate).toContain('cargo test');
    expect(pullRequestTemplate).toContain('No tokens');
    expect(pullRequestTemplate).toContain('version');
  });

  it('publishes a security policy with private reporting instructions', () => {
    expect(security).toContain('Report a vulnerability');
    expect(security).toContain('Supported versions');
    expect(security).toContain('Do not open a public issue');
  });

  it('links users to bugs, features, contributing and security', () => {
    for (const target of [
      'CONTRIBUTING.md',
      'SECURITY.md',
      'issues/new',
      'bug_report.yml',
      'feature_request.yml',
    ]) {
      expect(readme, target).toContain(target);
    }
  });

  it('links to the local documentation files shipped with the repository', () => {
    for (const target of [
      'CONTRIBUTING.md',
      'SECURITY.md',
      'CODE_OF_CONDUCT.md',
      'LICENSE',
      'THIRD_PARTY_NOTICES.md',
      'docs/',
    ]) {
      expect(readme, target).toContain(`](${target}`);
    }
    for (const target of ['CODE_OF_CONDUCT.md', 'SECURITY.md', 'LICENSE']) {
      expect(contributing, target).toContain(`](${target})`);
    }
    expect(security).toContain('](CONTRIBUTING.md)');
  });

  it('points the issue chooser at the real repository pages', () => {
    expect(issueConfig).toContain('https://github.com/veb898-cyber/VoidLauncher#readme');
    expect(issueConfig).toContain('https://github.com/veb898-cyber/VoidLauncher/security/policy');
    expect(issueConfig).toContain(
      'https://github.com/veb898-cyber/VoidLauncher/releases/latest',
    );
  });
});
