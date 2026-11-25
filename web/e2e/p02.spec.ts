import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, partiesRegion } from './parties';

test('p02_related_and_existing_edges', async ({ page }) => {
  await page.goto('/parties');
  await addParty(page, 'Fieldwork', 'org');
  const workspace = partiesRegion(page);
  const sheet = workspace.getByRole('region', { name: 'Party sheet', exact: true });
  await workspace.getByRole('button', { name: 'Sources inbox' }).click();
  await workspace.getByLabel('Captured text').fill('Fieldwork publishes the contributor guide. Mira maintains Fieldwork.');
  await workspace.getByRole('button', { name: 'Save source' }).click();
  await expect(workspace.getByText(/Source saved independently|Extractor is analyzing the saved source|Extractor finished/)).toBeVisible();

  const graph = workspace.getByRole('region', { name: 'Party network' });
  await graph.getByText('Relationships and evidence').click();
  const related = graph.getByRole('form', { name: 'Add a source-backed related entity' });
  await related.getByLabel('New entity type').selectOption('source');
  await related.getByLabel('Name', { exact: true }).fill('Contributor guide');
  await related.getByLabel('Saved source').selectOption({ index: 1 });
  await related.getByLabel('Exact supporting quote').fill('Fieldwork publishes the contributor guide.');
  await related.getByRole('textbox', { name: 'Initial owner notes', exact: true }).fill('Use **small fixtures**.');
  await related.getByText('Preview Markdown', { exact: true }).click();
  await expect(related.locator('strong')).toHaveText('small fixtures');
  await related.getByRole('button', { name: 'Save related entity and relationship' }).click();
  const editor = workspace.getByRole('region', { name: 'Card editor' });
  await expect(editor.getByRole('heading', { name: 'Edit card · Contributor guide' })).toBeVisible();
  await expect(editor.getByLabel('Owner notes')).toContainText('Use **small fixtures**.');
  await expect(sheet.getByRole('button', { name: 'Fieldwork', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await expect(sheet.getByRole('button', { name: 'Contributor guide', exact: true })).toHaveCount(0);

  await editor.getByRole('button', { name: 'Cancel', exact: true }).click();
  await graph.getByRole('button', { name: /Fieldwork publishes Contributor guide/ }).click();
  await workspace.getByRole('button', { name: 'Remove link' }).click();
  await workspace.getByRole('button', { name: 'Confirm removal' }).click();
  await expect(workspace.getByText('Relationship removed. Both cards remain in the workspace.')).toBeVisible();
  await expect(graph.getByRole('button', { name: /Fieldwork publishes Contributor guide/ })).toHaveCount(0);
});
