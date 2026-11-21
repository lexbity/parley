/**
 * Top-level owner tabs in the mandated desktop order (RFC-0001 APP-01):
 * "Desktop tabs are ordered Resources, Leads, Queue, Output, Settings."
 */
export const tabs = ['Resources', 'Leads', 'Queue', 'Output', 'Settings'] as const;

export type Tab = (typeof tabs)[number];
