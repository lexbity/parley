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
  await board.getByRole('button', { name: '+ Add card' }).nth(1).click();
  const editor = page.getByRole('form', { name: 'Resource card editor' });
  await expect(editor).toBeVisible();
  await editor.getByLabel('Title').fill('Importer checklist');
  await editor.getByLabel('Type').selectOption('artifact');
  await editor.getByLabel('Owner notes').fill('- try a multiline field\n- compare parsed rows');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(editor).not.toBeVisible();

  const tile = board.getByRole('button', { name: /Importer checklist/ }).first();
  await expect(tile).toBeVisible();
  await expect(board.getByText(/0 files · 0 linked/)).toBeVisible();

  // Selecting the tile sets the graph node and inspector.
  await tile.click();
  const canvas = page.getByTestId('resource-canvas');
  await expect(canvas).toBeVisible();
  await expect(page.getByRole('region', { name: 'Resource inspector' }).getByText('Importer checklist')).toBeVisible();

  // Shared editor: rename from the board.
  await board.getByRole('button', { name: 'Edit' }).first().click();
  await editor.getByLabel('Title').fill('Importer checklist v2');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(board.getByRole('button', { name: /Importer checklist v2/ })).toBeVisible();

  // Move to Available through the accessible column control.
  const moveSelect = board.getByRole('combobox', { name: /Move Importer checklist v2 to column/ });
  await moveSelect.selectOption('available');
  await expect(board.getByRole('heading', { name: /Available/ }).locator('..').getByText('Importer checklist v2')).toBeVisible();

  // Remove from board; the resource stays discoverable in the picker.
  await board.getByRole('button', { name: 'Remove' }).first().click();
  const picker = page.getByRole('region', { name: 'Resource inspector' }).getByRole('list', { name: 'Resource picker' });
  await expect(picker.getByText(/Importer checklist v2/)).toBeVisible();
  await expect(picker.getByText('off board')).toBeVisible();

  // Graph flow: create a related card from the node editor.
  await picker.getByRole('button', { name: /Importer checklist v2/ }).click();
  await canvas.locator('.vue-flow__node').first().click();
  await page.getByRole('button', { name: 'Edit card' }).click();
  await editor.locator('details.relation-box summary').click();
  await editor.getByRole('checkbox', { name: 'Include a relationship in this save' }).check();
  await editor.getByLabel('Link existing').selectOption('create');
  await editor.getByLabel('New card title').fill('Fixture walkthrough');
  await editor.getByLabel('Verb').fill('includes');
  await editor.getByLabel('Basis').fill('Owner linked the checklist fixture');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(page.getByRole('status').last()).toContainText('Card and relationship saved');

  // The related card is off board and the owner edge is visible in the
  // semantic list.
  await expect(picker.getByText(/Fixture walkthrough/)).toBeVisible();
  const relationships = page.getByRole('region', { name: 'Connections graph' }).getByRole('list', { name: 'Semantic relationship list' });
  await expect(relationships.getByText(/Importer checklist v2 includes Fixture walkthrough/)).toBeVisible();
});
