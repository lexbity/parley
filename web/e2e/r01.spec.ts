import { expect } from '@playwright/test';
import { test } from './fixtures';

/**
 * R01: the resource board journey (amendment 2026-11-22). Covers column
 * entry, shared editor commits, tile selection, moves, removal, the
 * off-board picker and a graph-created related card.
 */

test('r01_board_create_edit_move_remove_and_related_card', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();

  const board = page.getByRole('region', { name: 'Resource board' });
  await expect(board.getByRole('heading', { name: /Developing/ })).toBeVisible();
  await expect(board.getByRole('heading', { name: /Available/ })).toBeVisible();

  // Column entry: create a card in Developing with Markdown notes.
  await board.getByRole('button', { name: 'Add card to Developing' }).click();
  const editor = page.getByRole('form', { name: 'Resource card editor' });
  await expect(editor).toBeVisible();
  await editor.getByLabel('Title').fill('Importer checklist');
  await editor.getByLabel('Type').selectOption('artifact');
  await editor.getByLabel('Owner notes').fill('- try a multiline field\n- compare parsed rows');
  await editor.getByLabel('Add files').setInputFiles({ name: 'fixture.txt', mimeType: 'text/plain', buffer: Buffer.from('fixture body') });
  await expect(editor.getByText('Ready to attach', { exact: false })).toBeVisible();
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(editor).not.toBeVisible();

  const tile = board.getByRole('button', { name: /Importer checklist/ }).first();
  await expect(tile).toBeVisible();
  await expect(board.getByText(/1 file · 0 linked/)).toBeVisible();

  // Selecting the tile sets the graph node and inspector.
  await tile.click();
  const canvas = page.getByTestId('resource-canvas');
  await expect(canvas).toBeVisible();
  await expect(page.getByRole('region', { name: 'Resource inspector' }).getByText('Importer checklist')).toBeVisible();

  // Shared editor: rename from the board.
  await board.getByRole('button', { name: 'Edit Importer checklist', exact: true }).click();
  await editor.getByLabel('Title').fill('Importer checklist v2');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(board.getByRole('button', { name: /^(?:Artifact )?Importer checklist v2/ })).toBeVisible();

  // Move to Available through the accessible column control.
  const details = page.getByRole('region', { name: 'Resource inspector' });
  const moveSelect = details.getByRole('combobox', { name: /Move Importer checklist v2 to column/ });
  await moveSelect.selectOption('available');
  await expect(board.getByRole('heading', { name: /Available/ }).locator('..').getByText('Importer checklist v2')).toBeVisible();

  // Remove from board; the resource stays discoverable in the picker.
  await details.getByRole('button', { name: 'Remove from board' }).click();
  await board.getByRole('button', { name: 'Find resource' }).click();
  const picker = page.getByRole('list', { name: 'Resource picker' });
  await expect(picker.getByText(/Importer checklist v2/)).toBeVisible();
  await expect(picker.getByText('off board')).toBeVisible();

  // Graph flow: create a related card from the node editor.
  await picker.getByRole('button', { name: /^(?:Artifact )?Importer checklist v2/ }).click();
  await canvas.locator('.vue-flow__node').first().click();
  await expect(editor).toBeVisible();
  await editor.locator('details.relation-box summary').click();
  await editor.getByRole('checkbox', { name: 'Include a relationship in this save' }).check();
  await editor.getByLabel('Relationship target').selectOption('create');
  await editor.getByLabel('New card title').fill('Fixture walkthrough');
  await editor.getByLabel('Relationship verb').selectOption('includes');
  await editor.getByLabel('Basis').fill('Owner linked the checklist fixture');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Card and relationship saved' })).toBeVisible();

  // The related card is off board and the owner edge is visible in the
  // semantic list.
  await board.getByRole('button', { name: 'Find resource' }).click();
  await expect(picker.getByText(/Fixture walkthrough/)).toBeVisible();
  await page.getByRole('dialog', { name: 'Find resource' }).getByRole('button', { name: 'Close' }).click();
  await page.getByText('Cards and relationships', { exact: true }).click();
  const relationships = page.getByRole('region', { name: 'Connections graph' }).getByRole('list', { name: 'Semantic relationship list' });
  await expect(relationships.getByText(/Importer checklist v2 includes Fixture walkthrough/)).toBeVisible();
});

test('narrow Resource views preserve a graph draft and return keyboard focus after closing', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/resources');
  const board = page.getByRole('region', { name: 'Resource board' });
  const views = page.getByRole('navigation', { name: 'Resources views' });
  const editor = page.getByRole('form', { name: 'Resource card editor' });

  await board.getByRole('button', { name: 'Add card to Inbox' }).click();
  await editor.getByLabel('Title').fill('Mobile checklist');
  await editor.getByRole('button', { name: 'Save card' }).click();
  const details = page.getByRole('region', { name: 'Resource inspector' });
  await expect(details.getByRole('heading', { name: 'Mobile checklist' })).toBeFocused();
  await views.getByRole('button', { name: 'Connections' }).click();

  const canvas = page.getByTestId('resource-canvas');
  await canvas.focus();
  await page.keyboard.press('Enter');
  await editor.getByLabel('Title').fill('Unsaved mobile edit');
  await page.setViewportSize({ width: 1440, height: 850 });
  await expect(editor.getByLabel('Title')).toHaveValue('Unsaved mobile edit');
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(editor.getByLabel('Title')).toHaveValue('Unsaved mobile edit');

  await page.keyboard.press('Escape');
  const draft = page.getByRole('alertdialog', { name: 'Unsaved card draft' });
  await expect(draft).toBeVisible();
  await draft.getByRole('button', { name: 'Stay' }).click();
  await expect(editor.getByLabel('Title')).toHaveValue('Unsaved mobile edit');
  await page.keyboard.press('Escape');
  await draft.getByRole('button', { name: 'Discard unsaved changes' }).click();
  await expect(editor).toBeHidden();
  await expect(canvas).toBeFocused();

  await page.reload();
  await expect(views.getByRole('button', { name: 'Connections' })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.getByRole('heading', { name: 'Mobile checklist · connections' })).toBeVisible();
});
