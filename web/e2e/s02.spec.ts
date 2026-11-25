import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties, partiesRegion } from './parties';

test('s02_independent_source_same_name_parties_graph_evidence_and_correction', async ({ page }) => {
  await page.goto('/');
  await openParties(page);
  const suffix = randomUUID();
  const orgName = `Northline ${suffix}`;
  const dana = `Dana Lee ${suffix}`;

  // Same-name parties stay distinct; no identity merge is inferred from a label.
  await addParty(page, orgName, 'org');
  await addParty(page, dana, 'person');
  await addParty(page, dana, 'person');
  const region = partiesRegion(page);
  await expect(region.getByRole('button', { name: dana })).toHaveCount(2);

  // A saved source is independent: no person is inferred or linked.
  await region.getByRole('navigation', { name: 'Lead workspaces' }).getByRole('button', { name: 'Sources inbox' }).click();
  const sourceText = `The next engineer ${suffix} will work with Dana on onboarding.`;
  await region.getByLabel('Captured text').fill(sourceText);
  await region.getByRole('button', { name: 'Save source' }).click();
  await expect(region.getByText(/Source saved independently|Extractor is analyzing the saved source|Extractor finished/)).toBeVisible();

  // Select the organization row; the graph becomes source-backed relative to it.
  await region.getByRole('button', { name: orgName }).click();
  const graph = page.getByRole('region', { name: 'Party network' });
  await graph.getByText('Relationships and evidence').click();
  const form = graph.getByRole('form', { name: 'Add a source-backed related entity' });
  await form.getByLabel('Related to').selectOption({ label: `${orgName} · org` });
  await form.getByLabel('New entity type').selectOption({ label: 'Person (member of organization)' });
  await form.getByLabel('Name').fill(dana);
  await form.getByLabel('Saved source').selectOption({ index: 1 });
  await form.getByLabel('Exact supporting quote').fill('will work with Dana on onboarding');
  await form.getByRole('button', { name: 'Save related entity and relationship' }).click();
  await expect(page.getByText('The entity and source-attributed directed relationship were saved together.')).toBeVisible();

  await region.getByRole('region', { name: 'Card editor' }).getByRole('button', { name: 'Cancel', exact: true }).click();

  // The graph shows the directed edge; the sheet keeps both same-name rows and
  // the organization as the highlighted root.
  await expect(page.getByRole('button', { name: new RegExp(`${dana}.*member_of.*${orgName}`) })).toBeVisible();
  await expect(region.locator('tbody .row-party').filter({ hasText: dana })).toHaveCount(3);
  await expect(region.getByRole('button', { name: orgName, exact: true })).toHaveAttribute('aria-pressed', 'true');

  // Reload restores the deep-linked Parties tab and the persisted rows.
  await page.reload();
  await openParties(page);
  await expect(region.getByRole('button', { name: orgName, exact: true })).toBeVisible();
  await expect(region.locator('tbody .row-party').filter({ hasText: dana })).toHaveCount(3);
});
