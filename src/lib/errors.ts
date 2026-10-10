/**
 * Friendly error messages (roadmap stage 6, "better error UX").
 *
 * Backend commands return raw English strings; many of them describe a
 * handful of recurring situations (network down, access denied, disk
 * full…). `friendlyErrorMessage` maps those to localized one-liners and
 * falls back to the raw message for anything unrecognized — the user
 * never loses information, common failures just stop reading like
 * stack traces.
 */
import type { MessageKey } from './i18n';

type Translate = (key: MessageKey, vars?: Record<string, string | number>) => string;

interface Rule {
  test: RegExp;
  key: MessageKey;
}

// Ordered: the first match wins (e.g. a download timeout is a network
// problem even when the message mentions Java).
const RULES: Rule[] = [
  { test: /timed? ?out|timeout|connection (reset|refused|aborted|closed)|dns|unreachable|no such host|failed to lookup|network|proxy/i, key: 'errors.network' },
  { test: /access is denied|access denied|permission denied|\(os error 5\)/i, key: 'errors.access' },
  { test: /no space left|not enough (disk )?space|disk full/i, key: 'errors.disk' },
  { test: /could not find or load main class/i, key: 'errors.main_class' },
  { test: /unable to access jarfile|failed to detect java|java runtime (not found|missing)|cannot find java/i, key: 'errors.java_missing' },
  { test: /already running/i, key: 'errors.already_running' },
];

/** Localize a known backend error; unknown errors pass through unchanged. */
export function friendlyErrorMessage(err: unknown, t: Translate): string {
  const raw = err instanceof Error ? err.message : String(err);
  for (const rule of RULES) {
    if (rule.test.test(raw)) return t(rule.key);
  }
  return raw;
}
