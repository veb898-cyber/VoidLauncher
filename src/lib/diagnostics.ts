/** Shared types + i18n key maps for the diagnostics / crash analyzer UI. */
import type { MessageKey } from './i18n';

export interface DiagCheck {
  id: string;
  status: 'ok' | 'warn' | 'error' | 'info';
  value: string;
}

export interface DiagIssue {
  id: string;
  severity: 'warn' | 'error';
  params: Record<string, string>;
}

export interface DiagInstanceInfo {
  name: string;
  mc_version: string;
  loader: string;
  loader_version: string | null;
}

export interface VerifyIssue {
  path: string;
  kind: string;
  repairable: boolean;
  /** Extra context for the kind ("outdated_loader" carries the loader range). */
  detail?: string | null;
}

export interface VerifyCategory {
  id: string;
  checked: number;
  issue_count: number;
  issues: VerifyIssue[];
  /** Files proven against a trusted checksum in this scan. */
  verified: number;
  /** Files that exist but have no trusted checksum to prove them. */
  unverified: number;
  /** `verified` is a local sidecar fingerprint, not external provenance. */
  fingerprint_only?: boolean;
}

export interface VerifyReport {
  categories: VerifyCategory[];
  repairable_count: number;
  content_issues: number;
  healthy: boolean;
  mode: 'quick' | 'deep';
}

export interface DiagReport {
  launcher_version: string;
  checks: DiagCheck[];
  issues: DiagIssue[];
  instance: DiagInstanceInfo | null;
  verify: VerifyReport | null;
}

export interface CrashAction {
  kind: 'disable_mod' | 'open_url' | 'repair_files' | string;
  arg: string;
  tone: 'danger' | 'secondary' | string;
}

export interface CrashAnalysis {
  verdict: 'likely' | 'possible' | 'unknown';
  kind: string;
  culprit: string | null;
  culprit_file?: string | null;
  culprit_candidates?: string[];
  detail: string;
  root_cause?: string | null;
  exit_code?: number | null;
  actions?: CrashAction[];
  /**
   * Where the verdict came from. `'clean'` = the analyzed run exited with
   * code 0 and left no crash artifacts — NOT a crash at all.
   */
  source: 'crash-report' | 'session-log' | 'latest-log' | 'hs-error' | 'clean' | 'none';
  report_path: string | null;
}

/** Check id → i18n label. */
export const CHECK_LABELS: Record<string, MessageKey> = {
  os: 'diag.check_os',
  cpu: 'diag.check_cpu',
  ram: 'diag.check_ram',
  gpu: 'diag.check_gpu',
  disk: 'diag.check_disk',
  java: 'diag.check_java',
  network: 'diag.check_network',
  launcher: 'diag.check_launcher',
  instance: 'diag.check_instance',
  files: 'diag.check_files',
};

/** Verify category id → existing i18n label. */
export const VERIFY_CATEGORY_LABELS: Record<string, MessageKey> = {
  metadata: 'verify.cat_metadata',
  minecraft: 'verify.cat_minecraft',
  libraries: 'verify.cat_libraries',
  assets: 'verify.cat_assets',
  loader: 'verify.cat_loader',
  java: 'verify.cat_java',
  content: 'verify.cat_content',
};

/** Issue id → message / hint pairs. */
export const ISSUE_KEYS: Record<string, { message: MessageKey; fix: MessageKey }> = {
  disk_free: { message: 'diag.issue_disk_free', fix: 'diag.fix_disk_free' },
  memory: { message: 'diag.issue_memory', fix: 'diag.fix_memory' },
  loader_missing: { message: 'diag.issue_loader_missing', fix: 'diag.fix_loader' },
  java_mismatch: { message: 'diag.issue_java_mismatch', fix: 'diag.fix_java_mismatch' },
  java_unusable: { message: 'diag.issue_java_unusable', fix: 'diag.fix_java_unusable' },
  java_missing: { message: 'diag.issue_java_missing', fix: 'diag.fix_java_missing' },
  files: { message: 'diag.issue_files', fix: 'diag.fix_files' },
  content: { message: 'diag.issue_content', fix: 'diag.fix_content' },
  network: { message: 'diag.issue_network', fix: 'diag.fix_network' },
};

/** Crash kind → message / optional hint. */
export const CRASH_KEYS: Record<string, { message: MessageKey; hint?: MessageKey }> = {
  oom: { message: 'diag.crash_oom', hint: 'diag.crash_hint_oom' },
  java_version: { message: 'diag.crash_java_version', hint: 'diag.crash_hint_java' },
  jvm_args: { message: 'diag.crash_jvm_args', hint: 'diag.crash_hint_jvm' },
  launch_config: { message: 'diag.crash_launch_config', hint: 'diag.crash_hint_launch' },
  mod_crash: { message: 'diag.crash_mod_crash', hint: 'diag.crash_hint_mod' },
  mod_dependency: { message: 'diag.crash_mod_dependency', hint: 'diag.crash_hint_dependency' },
  loader_outdated: { message: 'diag.crash_loader_outdated', hint: 'diag.crash_hint_loader' },
  missing_dependency: { message: 'diag.crash_missing_dependency', hint: 'diag.crash_hint_deps' },
  missing_class: { message: 'diag.crash_missing_class', hint: 'diag.crash_hint_mod' },
  mod_conflict: { message: 'diag.crash_mod_conflict', hint: 'diag.crash_hint_conflict' },
  graphics: { message: 'diag.crash_graphics', hint: 'diag.crash_hint_graphics' },
  system_memory: { message: 'diag.crash_system_memory', hint: 'diag.crash_hint_system_memory' },
  native_crash: { message: 'diag.crash_native', hint: 'diag.crash_hint_native' },
  graphics_driver: { message: 'diag.crash_graphics_driver', hint: 'diag.crash_hint_graphics_driver' },
  wrong_loader: { message: 'diag.crash_wrong_loader', hint: 'diag.crash_hint_wrong_loader' },
  wrong_game: { message: 'diag.crash_wrong_game', hint: 'diag.crash_hint_wrong_game' },
  duplicate_mod: { message: 'diag.crash_duplicate_mod', hint: 'diag.crash_hint_duplicate_mod' },
  mod_version_mismatch: { message: 'diag.crash_mod_version', hint: 'diag.crash_hint_mod_version' },
  unknown: { message: 'diag.crash_unknown', hint: 'diag.crash_hint_unknown' },
};

/** Crash action → button label and optional hint. */
export const CRASH_ACTION_KEYS: Record<string, { label: MessageKey; hint?: MessageKey }> = {
  disable_mod: { label: 'game_logs.action_disable_mod', hint: 'game_logs.action_disable_mod_hint' },
  open_url: { label: 'game_logs.action_driver', hint: 'game_logs.action_driver_hint' },
  repair_files: { label: 'game_logs.action_repair', hint: 'game_logs.action_repair_hint' },
};

/** Verdict → label. */
export const VERDICT_KEYS: Record<string, MessageKey> = {
  likely: 'diag.verdict_likely',
  possible: 'diag.verdict_possible',
  unknown: 'diag.verdict_unknown',
};
