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
  /** CurseForge file id — CF downloads are resolved from (project, file). */
  file_id?: string;
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

// ==================== CurseForge ====================

/** Raw CF search row (fields used by the browser). */
interface CfSearchRow {
  id: number;
  name: string;
  slug: string;
  summary: string;
  downloadCount: number;
  logo?: { thumbnailUrl?: string | null; url?: string | null };
  authors?: { name: string }[];
  dateModified?: string;
}

interface CfSearchResp {
  data: CfSearchRow[];
  pagination?: { index?: number; pageSize?: number; resultCount?: number; totalCount?: number };
}

interface CfFileRow {
  id: number;
  displayName: string;
  fileName: string;
  downloadUrl?: string | null;
  fileLength: number;
  fileDate: string;
  gameVersions: string[];
  releaseType?: number;
  hashes?: { value: string; algo: number }[];
  dependencies?: { modId: number; relationType: number }[];
}

interface CfFilesResp {
  data: CfFileRow[];
  pagination?: { index?: number; pageSize?: number; resultCount?: number; totalCount?: number };
}

interface CfDetailRow {
  name?: string;
  description?: string | null;
  logo?: { thumbnailUrl?: string | null; url?: string | null } | null;
}

const CF_LOADERS = ['Fabric', 'Forge', 'NeoForge', 'Quilt', 'Rift'];

function cfToProject(row: CfSearchRow, projectType: string): ProviderProject {
  return {
    project_id: String(row.id),
    title: row.name,
    description: row.summary ?? '',
    icon_url: row.logo?.thumbnailUrl || row.logo?.url || null,
    project_type: projectType,
    downloads: row.downloadCount ?? 0,
    slug: row.slug ?? '',
    author: row.authors?.[0]?.name,
    date_published: row.dateModified,
  };
}

function cfToPage(resp: CfSearchResp, params: ProviderSearchParams): ProviderSearchPage {
  return {
    hits: (resp.data ?? []).map((row) => cfToProject(row, params.projectType)),
    offset: resp.pagination?.index ?? params.offset,
    limit: resp.pagination?.pageSize ?? params.limit,
    total_hits: resp.pagination?.totalCount ?? (resp.data?.length ?? 0),
  };
}

/**
 * CurseForge as a first-class catalog. The version id is the structured
 * `cf:{project_id}:{file_id}` key the backend resolver understands; files
 * carry an empty URL when the signed chain must be resolved at download
 * time, plus the file id for `cmd_download_to_folder`.
 */
const curseforgeProvider: ContentProvider = {
  id: 'curseforge',

  async search(query, params) {
    const resp = await invoke<CfSearchResp>('cmd_search_curseforge', {
      query,
      mcVersion: params.mcVersion,
      loader: params.loader,
      projectType: params.projectType,
      index: params.index,
      offset: params.offset,
      limit: params.limit,
    });
    return cfToPage(resp, params);
  },

  async popular(params) {
    // Popularity = the same search with an empty query (backend sorts it by
    // downloads), keeping the per-project-type classId filter.
    const resp = await invoke<CfSearchResp>('cmd_search_curseforge', {
      query: '',
      mcVersion: params.mcVersion,
      loader: params.loader,
      projectType: params.projectType,
      index: params.index,
      offset: params.offset,
      limit: params.limit,
    });
    return cfToPage(resp, params);
  },

  async getProject(projectId) {
    const resp = await invoke<CfDetailRow>('cmd_get_curseforge_mod_detail', {
      modId: Number(projectId),
    });
    return {
      body: resp.description ?? '',
      title: resp.name,
      icon_url: resp.logo?.thumbnailUrl || resp.logo?.url || null,
    };
  },

  async getVersions(projectId, mcVersion, loader) {
    const resp = await invoke<CfFilesResp>('cmd_get_curseforge_files', {
      modId: Number(projectId),
      mcVersion,
      loader,
    });
    return (resp.data ?? []).map((file) => {
      const sha1 = file.hashes?.find((h) => h.algo === 1)?.value;
      const gameVersions = file.gameVersions ?? [];
      return {
        id: `cf:${projectId}:${file.id}`,
        project_id: projectId,
        name: file.displayName,
        version_number: file.displayName,
        game_versions: gameVersions.filter((v) => /^\d/.test(v)),
        loaders: gameVersions.filter((v) => CF_LOADERS.includes(v)),
        date_published: file.fileDate,
        files: [
          {
            // Empty url → the backend resolves the signed URL / CDN chain
            // from (project id, file id) at download time.
            url: file.downloadUrl || '',
            filename: file.fileName,
            primary: true,
            hashes: (sha1 ? { sha1 } : {}) as Record<string, string>,
            size: file.fileLength,
            file_id: String(file.id),
          },
        ],
        dependencies: (file.dependencies ?? [])
          .map((d): ProviderDependency | null => {
            const dependency_type =
              d.relationType === 2
                ? 'Required'
                : d.relationType === 3
                  ? 'Optional'
                  : d.relationType === 5
                    ? 'Incompatible'
                    : null;
            return dependency_type
              ? { project_id: String(d.modId), dependency_type }
              : null;
          })
          .filter((d): d is ProviderDependency => d !== null),
      };
    });
  },
};

const providers: Partial<Record<ProviderId, ContentProvider>> = {
  modrinth: modrinthProvider,
  curseforge: curseforgeProvider,
};

/** Registry lookup; unknown ids fall back to Modrinth. */
export function getProvider(id: ProviderId = 'modrinth'): ContentProvider {
  return providers[id] ?? providers.modrinth ?? modrinthProvider;
}
