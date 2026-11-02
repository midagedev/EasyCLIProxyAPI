import { resourceAgents, type McpEntry, type ResourceSnapshot } from '../services/agentResources';

export function createAgentResourcesMock(scenario: string) {
  const snapshot: ResourceSnapshot = scenario === 'empty' ? { skills: [], mcp: [], errors: [] } : {
    skills: [{ name: 'review-checklist', description: 'Review changes with a local checklist before shipping.', paths: ['/home/demo/.claude/skills/review-checklist'], source: null, managed: false, installed: { claude: true }, owned: {} }],
    mcp: [{ id: 'local-docs', agent: 'claude', enabled: true, spec: { type: 'stdio', command: 'docs-server', args: ['--local'], env: { DEMO_TOKEN: 'non-sensitive-fixture' } } }],
    errors: [],
  };
  return (command: string, payload: Record<string, unknown>): { value: unknown } | undefined => {
    if (!['get_agent_resources', 'update_agent_skill', 'update_agent_mcp'].includes(command)) return;
    if (scenario === 'error') throw new Error('Resource fixture read/write failed');
    if (command === 'get_agent_resources') return { value: structuredClone(snapshot) };
    const operation = payload.operation;
    if (!['create', 'edit', 'remove'].includes(String(operation))) throw new Error('Invalid resource operation');
    if (command === 'update_agent_skill') {
      const name = String(payload.name);
      const existing = snapshot.skills.find(skill => skill.name === name);
      if (operation === 'create' && existing?.managed) throw new Error('Skill identifier already managed; use Manage to edit it');
      if (operation !== 'create' && !existing?.managed) throw new Error('Unmanaged source directories cannot be edited or removed');
      if (existing && !existing.managed) throw new Error('Unmanaged skill target conflict');
      snapshot.skills = snapshot.skills.filter(skill => skill.name !== name);
      if (operation !== 'remove') {
        const targets = payload.targets as string[];
        const states = Object.fromEntries(resourceAgents.map(agent => [agent, targets.includes(agent)]));
        snapshot.skills.push({ name, description: existing?.description ?? 'Imported local Skill fixture', paths: existing?.paths ?? [`/home/demo/.config/easy-cli-proxy-api/agent-resources/skills/${name}`], source: existing?.source ?? String(payload.source), managed: true, installed: states, owned: states });
      }
    } else {
      const actual = snapshot.mcp.filter(entry => entry.id === payload.id);
      if (operation === 'create' && actual.length) throw new Error('Server identifier already exists; use Manage to edit it');
      if (operation !== 'create' && !actual.length) throw new Error('Server no longer exists; refresh before saving');
      if (JSON.stringify(actual) !== JSON.stringify(payload.expected)) throw new Error('Server changed since it was opened; refresh before saving');
      snapshot.mcp = snapshot.mcp.filter(entry => entry.id !== payload.id);
      if (operation !== 'remove') snapshot.mcp.push(...structuredClone(payload.entries as McpEntry[]));
    }
    return { value: null };
  };
}
