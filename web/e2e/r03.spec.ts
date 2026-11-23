import { expect } from '@playwright/test';
import { test } from './fixtures';

/**
 * R03: retirement lifecycle, cross-tab refresh and keyboard activation.
 */

test('r03_cross_tab_refresh_retire_and_keyboard_open', async ({ page, context }) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();

  const board = page.getByRole('region', { name: 'Resource board' });

  // Create a card on the first tab.
  await board.getByRole('button', { name: '+ Add card' }).first().click();
  const editor = page.getByRole('form', { name: 'Resource card editor' });
  await editor.getByLabel('Title').fill('Cross-tab card');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(board.getByRole('button', { name: /Cross-tab card/ })).toBeVisible();

  // The second tab in the same workspace sees the card via the board channel.
  const second = await context.newPage();
  await second.goto('/');
  await expect(second.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  const secondBoard = second.getByRole('region', { name: 'Resource board' });
  await expect(secondBoard.getByRole('button', { name: /Cross-tab card/ })).toBeVisible({ timeout: 10_000 });

  // Keyboard activation: focus the graph canvas and press Enter to open the
  // inspected resource in the editor.
  await second.locator('[data-testid="resource-canvas"]').focus();
  await second.keyboard.press('Enter');
  await expect(second.getByRole('form', { name: 'Resource card editor' })).toBeVisible();
  await second.getByRole('button', { name: 'Cancel' }).click();

  // Retire from the first tab; the card leaves both tabs' board and picker.
  await board.getByRole('button', { name: /Cross-tab card/ }).click();
  page.once('dialog', (dialog) => dialog.accept());
  const inspector = page.getByRole('region', { name: 'Resource inspector' });
  await inspector.getByRole('button', { name: 'Retire card' }).click();
  await expect(board.getByRole('button', { name: /Cross-tab card/ })).not.toBeVisible();
  await expect(inspector.getByRole('list', { name: 'Resource picker' }).getByText(/Cross-tab card/)).not.toBeVisible();
  await expect(secondBoard.getByRole('button', { name: /Cross-tab card/ })).not.toBeVisible({ timeout: 10_000 });
});
