/** Persisted override values for the instance editor's "use default" modes. */
export type GcChoice = 'inherit' | 'standard' | 'g1gc' | 'zgc';

export function memoryOverrideValue(
  useDefault: boolean,
  memoryMb: number,
): number | null {
  return useDefault ? null : memoryMb;
}

export function gcOverrideValue(gc: GcChoice): string | null {
  return gc === 'inherit' ? null : gc;
}
