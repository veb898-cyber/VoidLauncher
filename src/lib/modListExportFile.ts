/**
 * File-save, clipboard and template-editing helpers for the mod list export.
 *
 * The save flow is parameterized by injected dialog/writer functions so the
 * cancel and extension rules are testable without Tauri. The UI wires the
 * production implementations (plugin-dialog + `cmd_save_export_file`).
 */

import {
  exportExtension,
  normalizeExportExtension,
  type ModListFormat,
} from './modListExport';

export function sanitizeFileNamePart(value: string): string {
  const cleaned = value
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, '_')
    .replace(/[. ]+$/g, '')
    .trim()
    .slice(0, 80)
    .replace(/[. ]+$/g, '');
  return cleaned || 'instance';
}

/** `<instance> - mods.<ext>`; never a path, only a suggested file name. */
export function suggestedExportFilename(
  instanceName: string,
  extension: string,
): string {
  const extensionName = normalizeExportExtension(extension) || 'txt';
  return `${sanitizeFileNamePart(instanceName)} - mods.${extensionName}`;
}

/**
 * Appends the selected extension when the native dialog returned a name
 * without it. A user-typed extension is kept untouched rather than replaced.
 */
export function ensureExportExtension(path: string, extension: string): string {
  const extensionName = normalizeExportExtension(extension);
  if (!extensionName) return path;
  return path.toLowerCase().endsWith(`.${extensionName}`)
    ? path
    : `${path}.${extensionName}`;
}

/** Excel correctly detects UTF-8 CSV only with a BOM. */
export function shouldAddUtf8Bom(
  format: ModListFormat,
  customExtension: string,
): boolean {
  return exportExtension(format, customExtension) === 'csv';
}

export interface SaveModListRequest {
  content: string;
  instanceName: string;
  format: ModListFormat;
  customExtension: string;
}

export interface SaveModListDeps {
  /** Native save dialog. Receives the suggested name and the extension. */
  choosePath: (defaultPath: string, extension: string) => Promise<string | null>;
  /** Backend writer. `addUtf8Bom` is handled by the Rust command. */
  writeFile: (path: string, content: string, addUtf8Bom: boolean) => Promise<void>;
}

export type SaveModListResult =
  | { status: 'saved'; path: string; extension: string }
  | { status: 'cancelled' };

/**
 * Ask for a destination and write the generated text. Cancelling the native
 * dialog is a normal outcome and must not surface an error.
 */
export async function saveModListExport(
  request: SaveModListRequest,
  deps: SaveModListDeps,
): Promise<SaveModListResult> {
  const extension = exportExtension(request.format, request.customExtension);
  const defaultPath = suggestedExportFilename(request.instanceName, extension);
  const chosen = await deps.choosePath(defaultPath, extension);
  if (!chosen) return { status: 'cancelled' };

  const path = ensureExportExtension(chosen, extension);
  await deps.writeFile(path, request.content, shouldAddUtf8Bom(request.format, request.customExtension));
  return { status: 'saved', path, extension };
}

export interface ClipboardWriter {
  writeText(text: string): Promise<void>;
}

/**
 * Copy the complete generated result through the native Tauri clipboard
 * plugin. The writer is injected so tests can prove that this path never
 * touches `navigator.clipboard` (which would require a WebView permission
 * prompt and may include a read capability).
 */
export async function copyModListText(
  text: string,
  clipboard: ClipboardWriter,
): Promise<void> {
  if (!clipboard || typeof clipboard.writeText !== 'function') {
    throw new Error('Native clipboard writer is not available');
  }
  await clipboard.writeText(text);
}

/** Insert a variable token at the textarea selection, preserving both sides. */
export function insertTemplateVariable(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  token: string,
): { value: string; cursor: number } {
  const start = Math.max(0, Math.min(selectionStart, value.length));
  const end = Math.max(start, Math.min(selectionEnd, value.length));
  return {
    value: `${value.slice(0, start)}${token}${value.slice(end)}`,
    cursor: start + token.length,
  };
}
