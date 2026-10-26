import type { AppConfig } from '../../stores/settingsStore';

/** Apply a change to the settings draft without saving it yet. */
export type UpdateConfig = <K extends keyof AppConfig>(
  key: K,
  value: AppConfig[K],
) => void;
