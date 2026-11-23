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
  await board.getByRole('button', { name: 'More options' }).click();
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
  await expect(board.getByRole('region', { name: 'Available', exact: true }).getByText('Importer checklist v2')).toBeVisible();

  // Remove from board; the resource stays discoverable in the picker.
  await details.getByRole('button', { name: 'Remove from board' }).click();
  await board.getByRole('button', { name: 'Find resource' }).click();
  const picker = page.getByRole('list', { name: 'Resource picker' });
  await expect(picker.getByText(/Importer checklist v2/)).toBeVisible();
  await expect(picker.getByText('off board')).toBeVisible();

  // Graph flow: create a related card from the node editor.
  await picker.getByRole('button', { name: /^(?:Artifact )?Importer checklist v2/ }).click();
  const addRelationship = canvas.getByRole('button', { name: 'Add relationship from Importer checklist v2' });
  await addRelationship.focus();
  await addRelationship.press('Enter');
  await expect(editor).toBeVisible();
  await expect(editor.getByLabel('Relationship target')).toBeFocused();
  await editor.getByLabel('Relationship target').selectOption('create');
  await editor.getByLabel('New card title').fill('Fixture walkthrough');
  await editor.getByRole('textbox', { name: 'Related card notes' }).fill('Keep the **quoted newline**.');
  await editor.locator('.notes-block').filter({ hasText: 'Related card notes' }).getByText('Preview Markdown').click();
  await expect(editor.locator('strong').filter({ hasText: 'quoted newline' })).toBeVisible();
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
  const includes = relationships.getByText('Importer checklist v2 includes Fixture walkthrough');
  await expect(includes).toBeVisible();

  // With two links to the same card, remove only the relationship chosen.
  await addRelationship.click();
  await editor.getByLabel('Relationship target').selectOption('link');
  await editor.getByLabel('Existing card', { exact: true }).selectOption({ label: 'Fixture walkthrough · Artifact' });
  await editor.getByLabel('Relationship verb').selectOption('supports');
  await editor.getByLabel('Basis').fill('The walkthrough also supports the checklist');
  await editor.getByRole('button', { name: 'Save card' }).click();
  const supports = relationships.getByText('Importer checklist v2 supports Fixture walkthrough');
  await expect(supports).toBeVisible();
  const remove = canvas.getByRole('button', { name: 'Remove relationship to Fixture walkthrough' });
  await remove.click();
  const chooseRelationship = page.getByRole('dialog', { name: 'Remove a relationship' });
  await chooseRelationship.getByLabel('Relationship', { exact: true }).selectOption({ label: 'Importer checklist v2 includes Fixture walkthrough' });
  await chooseRelationship.getByRole('button', { name: 'Remove relationship' }).click();
  await expect(chooseRelationship).toBeHidden();
  await expect(includes).toHaveCount(0);
  await expect(supports).toBeVisible();

  page.once('dialog', dialog => dialog.accept());
  await remove.click();
  await expect(supports).toHaveCount(0);
  await expect(canvas.getByRole('button', { name: 'Open Importer checklist v2' })).toBeVisible();
  await board.getByRole('button', { name: 'Find resource' }).click();
  await expect(picker.getByText('Fixture walkthrough')).toBeVisible();
});

test('narrow Resource views preserve a graph draft and return keyboard focus after closing', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/resources');
  const board = page.getByRole('region', { name: 'Resource board' });
  const views = page.getByRole('navigation', { name: 'Resources views' });
  const editor = page.getByRole('form', { name: 'Resource card editor' });

  await board.getByRole('button', { name: 'Add card to Inbox' }).click();
  await board.getByRole('button', { name: 'More options' }).click();
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

test('resource lists and cards can be added inline, removed and recovered from the picker', async ({ page }) => {
  await page.goto('/resources');
  const board = page.getByRole('region', { name: 'Resource board' });
  await board.getByRole('button', { name: 'Add another list' }).click();
  const listName = board.getByRole('textbox', { name: 'List name' });
  await expect(listName).toBeFocused();
  await listName.fill('Workshop');
  await listName.press('Enter');
  const workshop = board.getByRole('region', { name: 'Workshop', exact: true });
  await expect(workshop).toBeVisible();
  await listName.press('Escape');

  await workshop.getByRole('button', { name: 'Add card to Workshop' }).click();
  const cardTitle = workshop.getByRole('textbox', { name: 'Card title in Workshop' });
  await expect(cardTitle).toBeFocused();
  await cardTitle.fill('Practice handout');
  await cardTitle.press('Enter');
  await expect(workshop.getByRole('button', { name: /^(?:Artifact )?Practice handout/ })).toBeVisible();
  await expect(cardTitle).toHaveValue('');
  await expect(cardTitle).toBeFocused();
  await cardTitle.press('Escape');
  await expect(workshop.getByRole('button', { name: 'Add card to Workshop' })).toBeFocused();

  page.once('dialog', dialog => dialog.accept());
  await workshop.getByRole('button', { name: 'Remove list Workshop' }).click();
  await expect(workshop).toHaveCount(0);
  await board.getByRole('button', { name: 'Find resource' }).click();
  const picker = page.getByRole('dialog', { name: 'Find resource' });
  const handout = picker.getByRole('listitem').filter({ hasText: 'Practice handout' });
  await expect(handout).toContainText('Off board');
  await handout.getByRole('combobox').selectOption('inbox');
  await picker.getByRole('button', { name: 'Close' }).click();

  const inbox = board.getByRole('region', { name: 'Inbox', exact: true });
  const removeCard = inbox.getByRole('button', { name: 'Delete Practice handout' });
  page.once('dialog', dialog => dialog.dismiss());
  await removeCard.click();
  await expect(removeCard).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await removeCard.click();
  await expect(removeCard).toHaveCount(0);
  await board.getByRole('button', { name: 'Find resource' }).click();
  await expect(handout).toHaveCount(0);
});
