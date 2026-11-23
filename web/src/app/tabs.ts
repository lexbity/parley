/**
 * Top-level owner tabs in the mandated desktop order (RFC-0001 APP-01):
 * "Desktop tabs are ordered Resources, Parties, Queue, Output, Settings."
 * The former Leads tab is renamed Parties (amendment 2026-11-22); legacy
 * /leads deep links resolve to this tab as aliases.
 */
export const tabs = ['Resources', 'Parties', 'Queue', 'Output', 'Settings'] as const;

export type Tab = (typeof tabs)[number];