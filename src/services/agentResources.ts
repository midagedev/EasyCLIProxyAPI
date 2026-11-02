import { invoke } from '@tauri-apps/api/core';

export const resourceAgents = ['claude', 'codex', 'gemini'] as const;
export type ResourceAgent = typeof resourceAgents[number];
export const resourceAgentNames: Record<ResourceAgent, string> = { claude: 'Claude Code', codex: 'Codex', gemini: 'Gemini CLI' };
export type LocalSkill = { name: string; description: string; paths: string[]; source: string | null; managed: boolean; installed: Partial<Record<ResourceAgent, boolean>>; owned: Partial<Record<ResourceAgent, boolean>> };
export type McpSpec = Record<string, unknown>;
export type McpEntry = { id: string; agent: ResourceAgent; enabled: boolean; spec: McpSpec };
export type ResourceSnapshot = { skills: LocalSkill[]; mcp: McpEntry[]; errors: string[] };
export const getAgentResources = () => invoke<ResourceSnapshot>('get_agent_resources');
export type ResourceOperation = 'create' | 'edit' | 'remove';
export const updateAgentSkill = (name: string, source: string | null, targets: ResourceAgent[], operation: ResourceOperation) => invoke<void>('update_agent_skill', { name, source, targets, operation });
export const updateAgentMcp = (id: string, entries: McpEntry[], expected: McpEntry[], operation: ResourceOperation) => invoke<void>('update_agent_mcp', { id, entries, expected, operation });
export const SECRET_MASK = '••••••••';

export function displayMcpSpec(spec: McpSpec, reveal: boolean): string {
  const copy = structuredClone(spec);
  if (!reveal) {
    for (const field of ['env', 'headers']) {
      const values = copy[field];
      if (values && typeof values === 'object' && !Array.isArray(values)) {
        copy[field] = Object.fromEntries(Object.keys(values).map(key => [key, SECRET_MASK]));
      }
    }
  }
  return JSON.stringify(copy, null, 2);
}

export function parseMcpSpec(text: string, original: McpSpec): McpSpec {
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('object');
  const spec = value as McpSpec;
  for (const field of ['env', 'headers']) {
    const values = spec[field];
    const previous = original[field] as Record<string, unknown> | undefined;
    if (values && typeof values === 'object' && !Array.isArray(values)) {
      for (const [key, item] of Object.entries(values)) {
        if (item === SECRET_MASK) {
          if (!previous || typeof previous[key] !== 'string') throw new Error('mask');
          (values as Record<string, unknown>)[key] = previous[key];
        }
      }
    }
  }
  return spec;
}

export function validateMcpSpec(spec: McpSpec, agent: ResourceAgent): boolean {
  if (spec.type === 'stdio') {
    if (typeof spec.command !== 'string' || !spec.command.trim() || 'url' in spec || 'httpUrl' in spec) return false;
  } else if (spec.type === 'http' || spec.type === 'sse') {
    if (agent === 'codex' && spec.type === 'sse') return false;
    try {
      const url = new URL(String(spec.url));
      if (!['http:', 'https:'].includes(url.protocol) || !url.hostname || url.username || url.password) return false;
    } catch { return false; }
    if ('command' in spec || 'args' in spec) return false;
  } else return false;
  if ('args' in spec && (!Array.isArray(spec.args) || !spec.args.every(item => typeof item === 'string'))) return false;
  for (const field of ['env', 'headers']) {
    const values = spec[field];
    if (field in spec && (!values || typeof values !== 'object' || Array.isArray(values) || !Object.values(values).every(item => typeof item === 'string'))) return false;
  }
  return !['httpUrl', 'http_headers', 'enabled'].some(field => field in spec);
}
