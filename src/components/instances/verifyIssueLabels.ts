import type { MessageKey } from '../../lib/i18n/en';

/**
 * Every issue kind currently emitted by Verify. The list is intentionally
 * explicit so a new backend kind without a frontend label is easy to spot in
 * review; unknown future kinds still have a readable runtime fallback.
 */
export const KNOWN_ISSUE_KINDS = [
  // Generic file checks.
  'missing',
  'corrupt',
  'invalid',
  'unsafe',
  'outdated_loader',
  'size_mismatch',
  'hash_mismatch',
  'incomplete',
  'unreadable',
  // Processor generation and installer provenance.
  'invalid_generation_identity',
  'missing_generation_metadata',
  'verification_incomplete',
  'unsafe_generation_cache',
  'pending_prepared_transaction',
  'pending_transaction_cleanup',
  'recovery_required',
  'corrupt_processor_journal',
  'missing_installer_metadata',
  'unreadable_installer_metadata',
  'stale_installer_metadata',
  'unreadable_installer',
  'corrupt_installer',
  'missing_install_profile',
  'invalid_install_profile',
  'missing_client_jar',
  'missing_generated_outputs',
  'invalid_processor_plan',
  'invalid_processor_metadata',
  'missing_processor_library',
  'missing_generated_output',
  'corrupt_generated_output',
  'unreadable_generated_output',
  'missing_generation_manifest',
  'invalid_generation_manifest',
  'missing_generation_record',
  'stale_generation_manifest',
  'migration_required',
  'generation_needs_repair',
  // Embedded libraries bound to the verified installer.
  'invalid_embedded_metadata',
  'unsafe_embedded_library',
  'missing_embedded_library',
  'corrupt_embedded_library',
  'unreadable_embedded_library',
  'installer_entry_missing',
] as const;

export type KnownIssueKind = (typeof KNOWN_ISSUE_KINDS)[number];

export const KIND_LABELS: Record<KnownIssueKind, MessageKey> = {
  missing: 'verify.kind_missing',
  corrupt: 'verify.kind_corrupt',
  invalid: 'verify.kind_invalid',
  unsafe: 'verify.kind_unsafe',
  outdated_loader: 'verify.kind_outdated_loader',
  size_mismatch: 'verify.kind_size_mismatch',
  hash_mismatch: 'verify.kind_hash_mismatch',
  incomplete: 'verify.kind_incomplete',
  unreadable: 'verify.kind_unreadable',
  invalid_generation_identity: 'verify.kind_invalid_generation_identity',
  missing_generation_metadata: 'verify.kind_missing_generation_metadata',
  verification_incomplete: 'verify.kind_verification_incomplete',
  unsafe_generation_cache: 'verify.kind_unsafe_generation_cache',
  pending_prepared_transaction: 'verify.kind_pending_prepared_transaction',
  pending_transaction_cleanup: 'verify.kind_pending_transaction_cleanup',
  recovery_required: 'verify.kind_recovery_required',
  corrupt_processor_journal: 'verify.kind_corrupt_processor_journal',
  missing_installer_metadata: 'verify.kind_missing_installer_metadata',
  unreadable_installer_metadata: 'verify.kind_unreadable_installer_metadata',
  stale_installer_metadata: 'verify.kind_stale_installer_metadata',
  unreadable_installer: 'verify.kind_unreadable_installer',
  corrupt_installer: 'verify.kind_corrupt_installer',
  missing_install_profile: 'verify.kind_missing_install_profile',
  invalid_install_profile: 'verify.kind_invalid_install_profile',
  missing_client_jar: 'verify.kind_missing_client_jar',
  missing_generated_outputs: 'verify.kind_missing_generated_outputs',
  invalid_processor_plan: 'verify.kind_invalid_processor_plan',
  invalid_processor_metadata: 'verify.kind_invalid_processor_metadata',
  missing_processor_library: 'verify.kind_missing_processor_library',
  missing_generated_output: 'verify.kind_missing_generated_output',
  corrupt_generated_output: 'verify.kind_corrupt_generated_output',
  unreadable_generated_output: 'verify.kind_unreadable_generated_output',
  missing_generation_manifest: 'verify.kind_missing_generation_manifest',
  invalid_generation_manifest: 'verify.kind_invalid_generation_manifest',
  missing_generation_record: 'verify.kind_missing_generation_record',
  stale_generation_manifest: 'verify.kind_stale_generation_manifest',
  migration_required: 'verify.kind_migration_required',
  generation_needs_repair: 'verify.kind_generation_needs_repair',
  invalid_embedded_metadata: 'verify.kind_invalid_embedded_metadata',
  unsafe_embedded_library: 'verify.kind_unsafe_embedded_library',
  missing_embedded_library: 'verify.kind_missing_embedded_library',
  corrupt_embedded_library: 'verify.kind_corrupt_embedded_library',
  unreadable_embedded_library: 'verify.kind_unreadable_embedded_library',
  installer_entry_missing: 'verify.kind_installer_entry_missing',
};

export function issueKindLabelKey(kind: string): MessageKey | undefined {
  return (KIND_LABELS as Record<string, MessageKey>)[kind];
}
