/// Whether the wizard still needs to request a loader-version page.
///
/// Forge/NeoForge lists are tied to the selected Minecraft version. If the
/// user picks a loader while the Minecraft version list is still loading,
/// the initial request is skipped; this helper lets the wizard retry once the
/// version arrives. `total === null` means no request has completed for the
/// current loader/version pair yet — a successful empty page (`total === 0`)
/// must not trigger an endless retry loop.
export function needsLoaderVersionFetch(params: {
  loader: string;
  mcVersion: string;
  total: number | null;
  loading: boolean;
  failed: boolean;
}): boolean {
  if (params.loader === 'Vanilla') return false;
  // Every loader request needs a Minecraft version: Fabric's backend also
  // rejects an empty one, so asking early would cache an empty result.
  if (params.mcVersion === '') return false;
  return params.total === null && !params.loading && !params.failed;
}
