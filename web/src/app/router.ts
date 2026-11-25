import { tabs, type Tab } from './tabs';

export type ParleyRoute = { tab: Tab; path: string; recordId: string | null; location: 'inbox' | 'templates' | 'workspace' };
const roots: Record<Tab, string> = { Resources: '/resources', Parties: '/parties', Queue: '/queue', Output: '/output', Settings: '/settings', Activity: '/activity' };

export function routeForTab(tab: Tab): string { return roots[tab]; }

export function resolveRoute(pathname: string): ParleyRoute {
  const path = pathname.replace(/\/{2,}/g, '/').replace(/\/$/, '') || '/';
  if (path === '/' || path === '/resources' || path.startsWith('/resources/')) return { tab: 'Resources', path: path === '/' ? '/resources' : path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  // Parties is the renamed Leads tab; legacy /leads deep links stay aliases
  // (amendment §1: "Preserve existing Leads deep links as aliases to Parties").
  if (path === '/leads/inbox' || path === '/parties/inbox') return { tab: 'Parties', path, recordId: null, location: 'inbox' };
  if (path === '/leads' || path.startsWith('/leads/') || path === '/parties' || path.startsWith('/parties/')) return { tab: 'Parties', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/queue' || path.startsWith('/queue/')) return { tab: 'Queue', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/output/templates') return { tab: 'Output', path, recordId: null, location: 'templates' };
  if (path === '/output' || path.startsWith('/output/')) return { tab: 'Output', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/settings' || path.startsWith('/settings/')) return { tab: 'Settings', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/activity' || path.startsWith('/activity/')) return { tab: 'Activity', path, recordId: null, location: 'workspace' };
  return { tab: tabs[0], path: '/resources', recordId: null, location: 'workspace' };
}
