/**
 * Top-level owner tabs in the mandated desktop order (RFC-0001 APP-01):
 * "Desktop tabs are ordered Resources, Parties, Queue, Output, Settings."
 * The former Leads tab is renamed Parties (amendment 2026-11-22); legacy
 * /leads deep links resolve to this tab as aliases. Activity (AR-001 AR09)
 * is the owner-wide work surface for jobs and delegated runs; it follows
 * Settings so the mandated order of the qualified product tabs is untouched.
 */
export const tabs = ['Resources', 'Parties', 'Queue', 'Output', 'Settings', 'Activity'] as const;

export type Tab = (typeof tabs)[number];
