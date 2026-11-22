import { tabs, type Tab } from './tabs';

export type ParleyRoute = { tab: Tab; path: string; recordId: string | null; location: 'inbox' | 'templates' | 'workspace' };
const roots: Record<Tab, string> = { Resources: '/resources', Leads: '/leads', Queue: '/queue', Output: '/output', Settings: '/settings' };

export function routeForTab(tab: Tab): string { return roots[tab]; }

export function resolveRoute(pathname: string): ParleyRoute {
  const path = pathname.replace(/\/{2,}/g, '/').replace(/\/$/, '') || '/';
  if (path === '/' || path === '/resources' || path.startsWith('/resources/')) return { tab: 'Resources', path: path === '/' ? '/resources' : path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/leads/inbox') return { tab: 'Leads', path, recordId: null, location: 'inbox' };
  if (path === '/leads' || path.startsWith('/leads/')) return { tab: 'Leads', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/queue' || path.startsWith('/queue/')) return { tab: 'Queue', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/output/templates') return { tab: 'Output', path, recordId: null, location: 'templates' };
  if (path === '/output' || path.startsWith('/output/')) return { tab: 'Output', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  if (path === '/settings' || path.startsWith('/settings/')) return { tab: 'Settings', path, recordId: path.split('/')[2] ?? null, location: 'workspace' };
  return { tab: tabs[0], path: '/resources', recordId: null, location: 'workspace' };
}
