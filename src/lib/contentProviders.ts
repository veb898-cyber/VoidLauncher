// Provider abstraction for the content catalog (stage 3, roadmap item 17).
//
// The browser UI talks to this interface instead of raw `invoke` calls, so
// adding CurseForge as a first-class catalog provider (stage 4) is a matter
// of implementing `ContentProvider` and registering it — no UI changes.

import { invoke } from '@tauri-apps/api/core';

export type ProviderId = 'modrinth' | 'curseforge';

/** Normalized compatibility states returned by the backend classifier. */
export type CompatState = 'compatible' | 'unverified' | 'incompatible' | 'unknown';

/** One catalog project, as shown in search/popular lists. */
export interface ProviderProject {
  project_id: string;
  title: string;
  description: string;
  icon_url: string | null;
  project_type: string;
  downloads: number;
  slug: string;
  author?: string;
  date_published?: string;
}

/** Project detail panel payload (description body, extra metadata). */
export interface ProviderProjectDetail {
  body?: string;
  description?: string;
  title?: string;
  icon_url?: string | null;
}

export interface ProviderFile {
  url: string;
  filename: string;
  primary?: boolean;
  hashes?: Record<string, string>;
  size?: number;
}

export interface ProviderDependency {
  project_id?: string | null;
  version_id?: string | null;
  dependency_type?: string;
}

export interface ProviderVersion {
  id: string;
  project_id: string;
  name: string;
  version_number: string;
  game_versions?: string[];
  loaders?: string[];
  date_published?: string;
  files: ProviderFile[];
  dependencies?: ProviderDependency[];
}

export interface ProviderSearchParams {
  projectType: string;
  mcVersion: string | null;
  loader: string | null;
  index: string;
  offset: number;
  limit: number;
}

export interface ProviderSearchPage {
  hits: ProviderProject[];
  offset: number;
  limit: number;
  total_hits: number;
}

/** A content catalog backend (search, popular, project detail, versions). */
export interface ContentProvider {
  readonly id: ProviderId;
  search(query: string, params: ProviderSearchParams): Promise<ProviderSearchPage>;
  popular(params: ProviderSearchParams): Promise<ProviderSearchPage>;
  getProject(projectId: string): Promise<ProviderProjectDetail>;
  getVersions(
    projectId: string,
    mcVersion: string | null,
    loader: string | null
  ): Promise<ProviderVersion[]>;
}

const modrinthProvider: ContentProvider = {
  id: 'modrinth',

  async search(query, params) {
    return invoke<ProviderSearchPage>('cmd_search_modrinth', {
      query,
      projectType: params.projectType,
      mcVersion: params.mcVersion,
      loader: params.loader,
      index: params.index,
      offset: params.offset,
      limit: params.limit,
    });
  },

  async popular(params) {
    return invoke<ProviderSearchPage>('cmd_popular_modrinth', {
      projectType: params.projectType,
      mcVersion: params.mcVersion,
      loader: params.loader,
      index: params.index,
      offset: params.offset,
      limit: params.limit,
    });
  },

  async getProject(projectId) {
    return invoke<ProviderProjectDetail>('cmd_get_modrinth_project', { id: projectId });
  },

  async getVersions(projectId, mcVersion, loader) {
    return invoke<ProviderVersion[]>('cmd_get_modrinth_versions', { projectId, mcVersion, loader });
  },
};

// CurseForge joins the registry in stage 4 — until then the lookup falls
// back to Modrinth for its id.
const providers: Partial<Record<ProviderId, ContentProvider>> = {
  modrinth: modrinthProvider,
};

/** Registry lookup; unknown ids fall back to Modrinth. */
export function getProvider(id: ProviderId = 'modrinth'): ContentProvider {
  return providers[id] ?? providers.modrinth ?? modrinthProvider;
}
