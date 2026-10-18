/** Parses a loader install progress message like "Downloading libraries
 *  (3/12)" into a short label and a 0..100 percent (-1 = indeterminate). */
export function parseLoaderProgress(message: string): { label: string; percent: number } {
  const match = message.match(/\((\d+)\/(\d+)\)/);
  if (match) {
    const current = parseInt(match[1], 10);
    const total = parseInt(match[2], 10);
    const shortName = message.replace(/\s*\(\d+\/\d+\)/, '').split(':').pop()?.trim() || message;
    return { label: `${shortName} (${current}/${total})`, percent: Math.round((current / total) * 100) };
  }
  return { label: message, percent: -1 };
}
