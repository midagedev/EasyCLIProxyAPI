import { useCallback, useEffect, useState } from 'react';
import { FolderOpen, Plus, RefreshCw, Search } from 'lucide-react';
import { useI18n } from '../i18n';
import { Dialog } from '../components/Dialog';
import { Notice } from '../components/Notice';
import { useConfirmation } from '../components/ConfirmationDialog';
import { APP_NAVIGATION_REQUEST_EVENT, type AppNavigationRequest } from '../navigation';
import { open } from '../services/platform';
import {
  displayMcpSpec, getAgentResources, parseMcpSpec, resourceAgentNames, resourceAgents,
  updateAgentMcp, updateAgentSkill, validateMcpSpec,
  type LocalSkill, type McpEntry, type McpSpec, type ResourceAgent, type ResourceSnapshot,
} from '../services/agentResources';
import './AgentResourcesPanel.css';

type TargetDraft = { selected: boolean; enabled: boolean; text: string; original: McpSpec; reveal: boolean };
type Editor = { name: string; path: string; skill?: LocalSkill; existing: boolean; expected: McpEntry[]; targets: Record<ResourceAgent, TargetDraft> };
const template = (type: string): McpSpec => type === 'stdio' ? { type, command: '', args: [], env: {} } : { type, url: '', headers: {} };

export function AgentResourcesPanel({ kind }: { kind: 'skills' | 'mcp' }) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<ResourceSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [success, setSuccess] = useState(false);
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState<ResourceAgent | ''>('');
  const [editor, setEditor] = useState<Editor | null>(null);
  const [editorError, setEditorError] = useState('');
  const { askConfirmation, confirmationDialog } = useConfirmation();
  const refresh = useCallback(async () => {
    setLoading(true);
    setError('');
    try { setSnapshot(await getAgentResources()); }
    catch (reason) { setError(String(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    const guard = (event: Event) => { if (editor || busy) event.preventDefault(); };
    const navigate = (event: Event) => {
      if (!editor && !busy) return;
      event.preventDefault();
      if (!busy) void askConfirmation({ title: t('resources.cancel'), message: t('agents.sessions.context.confirmDiscard') }).then(confirmed => {
        if (confirmed) { setEditor(null); (event as AppNavigationRequest).detail(); }
      });
    };
    window.addEventListener('beforeunload', guard);
    window.addEventListener(APP_NAVIGATION_REQUEST_EVENT, navigate);
    return () => { window.removeEventListener('beforeunload', guard); window.removeEventListener(APP_NAVIGATION_REQUEST_EVENT, navigate); };
  }, [editor, busy, askConfirmation, t]);

  const start = (skill?: LocalSkill, id?: string) => {
    const expected = snapshot?.mcp.filter(entry => entry.id === id) ?? [];
    setEditorError('');
    setSuccess(false);
    setEditor({ name: skill?.name ?? id ?? '', path: skill?.paths[0] ?? '', skill, existing: Boolean(skill?.managed || id), expected,
      targets: Object.fromEntries(resourceAgents.map(agent => {
        const entry = expected.find(item => item.agent === agent);
        const original = entry?.spec ?? expected[0]?.spec ?? template('stdio');
        return [agent, { selected: kind === 'skills' ? Boolean(skill?.owned[agent]) : Boolean(entry), enabled: entry?.enabled ?? true, original, text: displayMcpSpec(original, false), reveal: false }];
      })) as Editor['targets'],
    });
  };
  const changeTarget = (agent: ResourceAgent, patch: Partial<TargetDraft>) => setEditor(current => current ? { ...current, targets: { ...current.targets, [agent]: { ...current.targets[agent], ...patch } } } : current);
  const save = async () => {
    if (!editor) return;
    if (!/^[a-zA-Z0-9_.-]{1,100}$/.test(editor.name) || ['.', '..'].includes(editor.name)) { setEditorError(t('resources.invalid')); return; }
    const targets = resourceAgents.filter(agent => editor.targets[agent].selected);
    let entries: McpEntry[] = [];
    if (kind === 'mcp') {
      try {
        entries = targets.map(agent => {
          const draft = editor.targets[agent];
          const spec = parseMcpSpec(draft.text, draft.original);
          if (!validateMcpSpec(spec, agent)) throw new Error('validation');
          return { id: editor.name, agent, enabled: draft.enabled, spec };
        });
        if (!entries.length) throw new Error('targets');
      } catch { setEditorError(t('resources.invalid')); return; }
    }
    const removedTargets = kind === 'skills'
      ? resourceAgents.some(agent => editor.skill?.owned[agent] && !editor.targets[agent].selected)
      : editor.expected.some(entry => !entries.some(next => next.agent === entry.agent && next.enabled));
    if (removedTargets && !await askConfirmation({ title: t('resources.save'), message: t('resources.removeConfirm') })) return;
    setBusy(true);
    setEditorError('');
    try {
      if (kind === 'skills') await updateAgentSkill(editor.name, editor.path || null, targets);
      else await updateAgentMcp(editor.name, entries, editor.expected);
      setEditor(null);
      await refresh();
      setSuccess(true);
    } catch (reason) { setEditorError(String(reason)); }
    finally { setBusy(false); }
  };
  const remove = async (name: string) => {
    if (!await askConfirmation({ title: t('resources.delete'), message: t('resources.removeConfirm'), details: [{ label: t('resources.name'), value: name }], variant: 'danger' })) return;
    setBusy(true);
    setError('');
    setSuccess(false);
    try {
      if (kind === 'skills') await updateAgentSkill(name, null, [], true);
      else await updateAgentMcp(name, [], snapshot?.mcp.filter(entry => entry.id === name) ?? [], true);
      await refresh();
      setSuccess(true);
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  };
  const query = search.trim().toLowerCase();
  const allSkills = snapshot?.skills ?? [];
  const skills = allSkills.filter(skill => (!filter || skill.installed[filter] || skill.owned[filter]) && `${skill.name} ${skill.description} ${skill.paths.join(' ')} ${skill.source ?? ''}`.toLowerCase().includes(query));
  const allIds = [...new Set(snapshot?.mcp.map(entry => entry.id) ?? [])].sort();
  const ids = allIds.filter(id => (!filter || snapshot?.mcp.some(entry => entry.id === id && entry.agent === filter)) && id.toLowerCase().includes(query));
  const count = kind === 'skills' ? skills.length : ids.length;
  const total = kind === 'skills' ? allSkills.length : allIds.length;
  return <section className="resource-page" aria-busy={loading || busy}>
    <header className="resource-heading"><div><h1>{t(`resources.${kind}`)}</h1><p>{t('resources.hint')}</p></div></header>
    <Notice tone="info">{t(kind === 'skills' ? 'resources.skillHint' : 'resources.mcpHint')}</Notice>
    <div className="resource-toolbar">
      <label className="resource-search"><Search size={16} aria-hidden="true" /><input aria-label={t('resources.search')} placeholder={t('resources.search')} value={search} onChange={event => setSearch(event.target.value)} /></label>
      <select aria-label={t('resources.filter')} value={filter} onChange={event => setFilter(event.target.value as ResourceAgent | '')}><option value="">{t('resources.all')}</option>{resourceAgents.map(agent => <option key={agent} value={agent}>{resourceAgentNames[agent]}</option>)}</select>
      <button className="secondary-button" disabled={busy || loading} onClick={() => void refresh()}><RefreshCw size={16} />{t('resources.refresh')}</button>
      <button className="primary-button" disabled={busy || loading || !snapshot} onClick={() => start()}><Plus size={16} />{t(kind === 'skills' ? 'resources.import' : 'resources.add')}</button>
    </div>
    {error && <Notice tone="error">{error}</Notice>}
    {snapshot?.errors.map((message, index) => <Notice key={index} tone="warn">{message}</Notice>)}
    {success && <Notice tone="success">{t('resources.saved')}</Notice>}
    {loading ? <p role="status">{t('resources.loading')}</p> : !count && !error ? <div className="resource-empty">{t(total ? 'resources.noMatches' : 'resources.empty')}</div> : null}
    <div className="resource-list">{kind === 'skills' ? skills.map(skill => <article className="panel surface-panel resource-card" key={skill.name}>
      <div className="resource-card-heading"><div><h2>{skill.name}</h2><span>{t(skill.managed ? 'resources.managed' : 'resources.unmanaged')}</span></div>
        <div className="resource-actions"><button className="secondary-button" disabled={busy} onClick={() => start(skill)}>{t(skill.managed ? 'resources.edit' : 'resources.import')}</button>{skill.managed && <button className="danger-button" disabled={busy} onClick={() => void remove(skill.name)}>{t('resources.delete')}</button>}</div></div>
      <p>{skill.description}</p>
      {skill.source && <p>{t('resources.source')}: <code>{skill.source}</code></p>}
      {skill.paths.map(path => <code className="resource-path" key={path}>{path}</code>)}
      <div className="resource-statuses">{resourceAgents.map(agent => <span key={agent} data-enabled={Boolean(skill.installed[agent])}>{resourceAgentNames[agent]} · {t(skill.installed[agent] ? 'resources.installed' : 'resources.absent')}</span>)}</div>
    </article>) : ids.map(id => <article className="panel surface-panel resource-card" key={id}>
      <div className="resource-card-heading"><h2>{id}</h2><div className="resource-actions"><button className="secondary-button" disabled={busy} onClick={() => start(undefined, id)}>{t('resources.edit')}</button><button className="danger-button" disabled={busy} onClick={() => void remove(id)}>{t('resources.delete')}</button></div></div>
      <div className="resource-statuses">{resourceAgents.map(agent => {
        const entry = snapshot?.mcp.find(item => item.agent === agent && item.id === id);
        return <span key={agent} data-enabled={Boolean(entry?.enabled)}>{resourceAgentNames[agent]} · {entry ? `${String(entry.spec.type)} · ${t(entry.enabled ? 'resources.enabled' : 'resources.disabled')}` : t('resources.absent')}</span>;
      })}</div>
    </article>)}</div>
    {editor && <Dialog className="config-dialog resource-editor" title={t(kind === 'skills' ? 'resources.import' : 'resources.mcp')} busy={busy} closeDisabled={busy} onClose={() => setEditor(null)}
      actions={<><button className="secondary-button" disabled={busy} onClick={() => setEditor(null)}>{t('resources.cancel')}</button><button className="primary-button" disabled={busy} onClick={() => void save()}>{t('resources.save')}</button></>}>
      <label>{t('resources.name')}<input value={editor.name} disabled={busy || editor.existing} onChange={event => setEditor({ ...editor, name: event.target.value })} /></label>
      {kind === 'skills' && <>
        <label>{t('resources.path')}<input value={editor.path} disabled={busy || editor.skill?.managed} onChange={event => setEditor({ ...editor, path: event.target.value })} /></label>
        {!editor.skill?.managed && <button className="secondary-button" disabled={busy} onClick={() => void (async () => {
          try { const path = await open({ directory: true }); if (path) setEditor(current => current ? { ...current, path, name: current.name || path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || '' } : current); }
          catch (reason) { setEditorError(String(reason)); }
        })()}><FolderOpen size={16} />{t('resources.browse')}</button>}
        {editor.skill && !editor.skill.managed && <Notice tone="info">{t('resources.conflict')}</Notice>}
      </>}
      <fieldset disabled={busy}><legend>{t('resources.targets')}</legend>{resourceAgents.map(agent => {
        const draft = editor.targets[agent];
        const blocked = kind === 'skills' && Boolean(editor.skill?.installed[agent] && !editor.skill.owned[agent] && editor.name === editor.skill.name);
        return <div key={agent} className="resource-target">
          <label className="resource-check"><input type="checkbox" checked={draft.selected} disabled={blocked} onChange={event => changeTarget(agent, { selected: event.target.checked })} />{resourceAgentNames[agent]}{blocked ? ` · ${t('resources.unmanaged')}` : ''}</label>
          {kind === 'mcp' && draft.selected && <>
            <label className="resource-check"><input type="checkbox" checked={draft.enabled} onChange={event => changeTarget(agent, { enabled: event.target.checked })} />{t('resources.enabled')}</label>
            <label>{t('resources.transport')}<select aria-label={`${resourceAgentNames[agent]} ${t('resources.transport')}`} value="" onChange={event => {
              const type = event.target.value;
              if (type) void askConfirmation({ title: t('resources.transport'), message: t('resources.replace') }).then(confirmed => { if (confirmed) { const original = template(type); changeTarget(agent, { original, text: displayMcpSpec(original, draft.reveal) }); } });
            }}><option value="">—</option><option value="stdio">stdio</option><option value="http">HTTP</option><option value="sse" disabled={agent === 'codex'}>SSE{agent === 'codex' ? ' (—)' : ''}</option></select></label>
            <label>{t('resources.spec')} · {resourceAgentNames[agent]}<textarea spellCheck={false} aria-label={`${resourceAgentNames[agent]} ${t('resources.spec')}`} rows={9} value={draft.text} onChange={event => changeTarget(agent, { text: event.target.value })} /></label>
            <button className="secondary-button" onClick={() => {
              try { const original = parseMcpSpec(draft.text, draft.original); changeTarget(agent, { original, reveal: !draft.reveal, text: displayMcpSpec(original, !draft.reveal) }); }
              catch { setEditorError(t('resources.invalid')); }
            }}>{t(draft.reveal ? 'resources.hide' : 'resources.reveal')}</button>
          </>}
        </div>;
      })}</fieldset>
      {editorError && <Notice tone="error">{editorError}</Notice>}
    </Dialog>}
    {confirmationDialog}
  </section>;
}
