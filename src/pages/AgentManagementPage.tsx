import { useState } from 'react';
import { AgentsPage } from './AgentsPage';
import { AgentResourcesPanel } from './AgentResourcesPanel';
import { Tabs } from '../components/Tabs';
import { useI18n } from '../i18n';
import { requestAppNavigation } from '../navigation';

export function AgentManagementPage() {
  const { t } = useI18n();
  const [tab, setTab] = useState<'settings' | 'skills' | 'mcp'>('settings');
  return <div className="agent-management-page">
    <Tabs aria-label={t('resources.tabs')} className="secondary-page-tabs" value={tab}
      onChange={value => requestAppNavigation(() => setTab(value))}
      items={(['settings', 'skills', 'mcp'] as const).map(value => ({ value, label: t(`resources.${value}`), id: `resources-tab-${value}`, panelId: `resources-panel-${value}` }))} />
    <div role="tabpanel" id={`resources-panel-${tab}`} aria-labelledby={`resources-tab-${tab}`}>
      {tab === 'settings' ? <AgentsPage /> : <AgentResourcesPanel key={tab} kind={tab} />}
    </div>
  </div>;
}
