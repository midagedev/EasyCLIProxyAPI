import { describe, expect, test } from 'bun:test';
import { displayMcpSpec, parseMcpSpec, validateMcpSpec, SECRET_MASK } from '../src/services/agentResources';
import { createAgentResourcesMock } from '../src/mocks/agentResourcesMock';
import { agentResourcesEn, agentResourcesJa, agentResourcesZhCN, agentResourcesZhTW } from '../src/i18n/agentResources';

describe('local resource editor', () => {
  const spec = { type: 'stdio', command: 'fixture', args: [], env: { TOKEN: 'private-fixture' }, headers: { Authorization: 'header-fixture' }, custom: { keep: true } };
  test('masks env and headers without mutating or losing original values', () => {
    const text = displayMcpSpec(spec, false);
    expect(text).not.toContain('private-fixture');
    expect(text).not.toContain('header-fixture');
    expect(text).toContain(SECRET_MASK);
    expect(parseMcpSpec(text, spec)).toEqual(spec);
    expect(displayMcpSpec(spec, true)).toContain('private-fixture');
    expect(() => parseMcpSpec(text.replace('TOKEN', 'NEW_TOKEN'), spec)).toThrow();
  });
  test('rejects malformed JSON, invalid shapes and unsupported transport combinations', () => {
    for (const text of ['{', '[]', 'null', 'false']) expect(() => parseMcpSpec(text, spec)).toThrow();
    expect(validateMcpSpec(spec, 'claude')).toBe(true);
    expect(validateMcpSpec({ type: 'sse', url: 'https://example.invalid/events' }, 'codex')).toBe(false);
    expect(validateMcpSpec({ type: 'sse', url: 'https://example.invalid/events' }, 'gemini')).toBe(true);
    for (const invalid of [
      { type: 'http', url: 'file:///secret' }, { type: 'http', url: 'http://user:pass@localhost' },
      { type: 'http', url: 'http://localhost', command: 'bad' }, { type: 'stdio', command: '' },
      { type: 'stdio', command: 'fixture', args: 'invalid' }, { type: 'stdio', command: 'fixture', env: { key: false } },
    ]) expect(validateMcpSpec(invalid, 'claude')).toBe(false);
  });
  test('browser runtime persists resource mutations within a session', () => {
    const mock = createAgentResourcesMock('empty');
    mock('update_agent_skill', { name: 'fixture', source: '/fixtures/source', targets: ['codex'], operation: 'create' });
    expect(mock('get_agent_resources', {})?.value).toMatchObject({ skills: [{ name: 'fixture', installed: { codex: true } }] });
    mock('update_agent_skill', { name: 'fixture', operation: 'remove' });
    expect(mock('get_agent_resources', {})?.value).toEqual({ skills: [], mcp: [], errors: [] });
    expect(() => createAgentResourcesMock('error')('get_agent_resources', {})).toThrow();
  });
  test('duplicate Skill import leaves the session unchanged and only explicit edit changes targets', () => {
    const mock = createAgentResourcesMock('empty');
    mock('update_agent_skill', { name: 'fixture', source: '/fixtures/A', targets: ['claude'], operation: 'create' });
    const before = mock('get_agent_resources', {})?.value;
    expect(() => mock('update_agent_skill', { name: 'fixture', source: '/fixtures/B', targets: ['codex', 'gemini'], operation: 'create' })).toThrow('already managed');
    expect(mock('get_agent_resources', {})?.value).toEqual(before);
    expect(() => mock('update_agent_skill', { name: 'fixture', source: '/fixtures/B', targets: ['codex'] })).toThrow('operation');
    expect(mock('get_agent_resources', {})?.value).toEqual(before);
    mock('update_agent_skill', { name: 'fixture', source: null, targets: ['gemini'], operation: 'edit' });
    expect(mock('get_agent_resources', {})?.value).toMatchObject({ skills: [{ source: '/fixtures/A', installed: { claude: false, gemini: true } }] });
    expect(() => mock('update_agent_skill', { name: 'missing', targets: [], operation: 'edit' })).toThrow();
  });
  test('MCP creation rejects active and disabled identifiers even with matching expected entries', () => {
    const mock = createAgentResourcesMock('empty');
    for (const enabled of [true, false]) {
      const entries = [{ id: `docs-${enabled}`, agent: 'claude', enabled, spec: { type: 'stdio', command: 'fixture-A' } }];
      mock('update_agent_mcp', { id: entries[0].id, entries, expected: [], operation: 'create' });
      const before = mock('get_agent_resources', {})?.value;
      for (const expected of [[], entries]) {
        expect(() => mock('update_agent_mcp', { id: entries[0].id, entries: [], expected, operation: 'create' })).toThrow('already exists');
        expect(mock('get_agent_resources', {})?.value).toEqual(before);
      }
      mock('update_agent_mcp', { id: entries[0].id, entries, expected: entries, operation: 'edit' });
    }
  });
  test('all current locales include every resource label', () => {
    for (const locale of [agentResourcesZhCN, agentResourcesZhTW, agentResourcesJa]) {
      expect(Object.keys(locale).sort()).toEqual(Object.keys(agentResourcesEn).sort());
      expect(Object.values(locale).every(value => value.length > 0)).toBe(true);
    }
  });
});
