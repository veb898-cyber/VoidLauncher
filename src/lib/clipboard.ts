/**
 * Copy text to the system clipboard and report whether it actually worked.
 *
 * WebView2's `navigator.clipboard` is not always available (non-secure
 * context / permission policy), and the Tauri plugin is the authoritative
 * path inside the app. Callers must only show "Copied" after `true`; a
 * fire-and-forget write produced false success toasts.
 */
export async function copyTextToClipboard(text: string): Promise<boolean> {
  try {
    const { writeText } = await import('@tauri-apps/plugin-clipboard-manager');
    await writeText(text);
    return true;
  } catch {
    // Not running under Tauri, or the plugin rejected the write.
  }

  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Permission denied; try the legacy selection-based fallback below.
  }

  try {
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    area.style.pointerEvents = 'none';
    document.body.appendChild(area);
    area.select();
    const copied = document.execCommand('copy');
    document.body.removeChild(area);
    return copied;
  } catch {
    return false;
  }
}
