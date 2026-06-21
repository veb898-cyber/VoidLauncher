export interface BannerPreset {
  id: string;
  label: string;
  gradient: string;
}

export const BANNER_PRESETS: BannerPreset[] = [
  { id: 'neon-purple', label: 'Neon Purple', gradient: 'linear-gradient(135deg, #a855f7, #06b6d4)' },
  { id: 'cyber-blue', label: 'Cyber Blue', gradient: 'linear-gradient(135deg, #3b82f6, #ec4899)' },
  { id: 'sunset', label: 'Sunset', gradient: 'linear-gradient(135deg, #f97316, #ef4444)' },
  { id: 'forest', label: 'Forest', gradient: 'linear-gradient(135deg, #22c55e, #06b6d4)' },
  { id: 'ocean', label: 'Ocean', gradient: 'linear-gradient(135deg, #6366f1, #a855f7)' },
  { id: 'midnight', label: 'Midnight', gradient: 'linear-gradient(135deg, #1e293b, #334155)' },
  { id: 'lava', label: 'Lava', gradient: 'linear-gradient(135deg, #dc2626, #f97316)' },
  { id: 'arctic', label: 'Arctic', gradient: 'linear-gradient(135deg, #0ea5e9, #a5f3fc)' },
  { id: 'royal', label: 'Royal', gradient: 'linear-gradient(135deg, #7c3aed, #f59e0b)' },
  { id: 'mint', label: 'Mint', gradient: 'linear-gradient(135deg, #10b981, #2dd4bf)' },
  { id: 'cherry', label: 'Cherry', gradient: 'linear-gradient(135deg, #be123c, #f43f5e)' },
  { id: 'deep-sea', label: 'Deep Sea', gradient: 'linear-gradient(135deg, #1e3a5f, #0891b2)' },
];

export function isGradientBanner(banner: string | null): banner is string {
  return !!banner && banner.startsWith('gradient:');
}

export function getGradientValue(banner: string): string {
  const id = banner.replace('gradient:', '');
  const preset = BANNER_PRESETS.find((p) => p.id === id);
  return preset ? preset.gradient : 'linear-gradient(135deg, var(--primary), var(--secondary))';
}
