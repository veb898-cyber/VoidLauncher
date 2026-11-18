/**
 * Build an accessible name for a control that acts on one instance/session.
 * The visible label stays the primary name; the object is appended only when
 * it is known, so empty states never produce a dangling separator.
 */
export function labelWithInstance(
  label: string,
  instance: string | null | undefined,
): string {
  const name = instance?.trim();
  return name ? `${label}: ${name}` : label;
}
