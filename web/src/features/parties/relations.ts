/** Each assertion reads from the subject to the object. */
export const relationshipTypes = [
  { from: 'person', verb: 'member_of', to: 'org' },
  { from: 'org', verb: 'works_with', to: 'project' },
  { from: 'person', verb: 'offers_to', to: 'resource' },
  { from: 'org', verb: 'publishes', to: 'source' },
  { from: 'person', verb: 'publishes', to: 'source' },
  { from: 'org', verb: 'expresses', to: 'interest' },
  { from: 'person', verb: 'expresses', to: 'interest' },
  { from: 'person', verb: 'maintains', to: 'org' },
  { from: 'person', verb: 'maintains', to: 'project' },
  { from: 'artifact', verb: 'could_help', to: 'org' },
  { from: 'artifact', verb: 'could_help', to: 'person' },
] as const;

export function allowedAssertion(from: string, verb: string, to: string) {
  return relationshipTypes.some(type => type.from === from && type.verb === verb && type.to === to);
}

export function kindLabel(kind: string) {
  if (kind === 'org') return 'Organization';
  return kind[0]?.toUpperCase() + kind.slice(1);
}
