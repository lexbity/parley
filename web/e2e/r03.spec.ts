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
  await board.getByRole('region', { name: 'Inbox', exact: true }).getByRole('button', { name: 'More options' }).click();
  const editor = page.getByRole('form', { name: 'Resource card editor' });
  await editor.getByLabel('Title').fill('Cross-tab card');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(board.getByRole('button', { name: /Cross-tab card/ }).first()).toBeVisible();

  // The second tab in the same workspace sees the card via the board channel.
  const second = await context.newPage();
  await second.goto('/');
  await expect(second.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  const secondBoard = second.getByRole('region', { name: 'Resource board' });
  await expect(secondBoard.getByRole('button', { name: /Cross-tab card/ }).first()).toBeVisible({ timeout: 10_000 });

  // Keyboard activation: focus the graph canvas and press Enter to open the
  // inspected resource in the editor.
  await second.locator('[data-testid="resource-canvas"]').focus();
  await second.keyboard.press('Enter');
  await expect(second.getByRole('form', { name: 'Resource card editor' })).toBeVisible();
  await second.getByRole('button', { name: 'Cancel' }).click();

  // Retire from the first tab; the card leaves both tabs' board and picker.
  await board.getByRole('button', { name: /Cross-tab card/ }).first().click();
  page.once('dialog', (dialog) => dialog.accept());
  const inspector = page.getByRole('region', { name: 'Resource inspector' });
  await inspector.getByText('Readiness and card actions').click();
  await inspector.getByRole('button', { name: 'Archive card' }).click();
  await expect(board.getByRole('button', { name: /Cross-tab card/ }).first()).not.toBeVisible();
  await board.getByRole('button', { name: 'Find resource' }).click();
  await expect(page.getByRole('dialog', { name: 'Find resource' }).getByText(/Cross-tab card/)).not.toBeVisible();
  await expect(secondBoard.getByRole('button', { name: /Cross-tab card/ }).first()).not.toBeVisible({ timeout: 10_000 });
});

test('r03_archive_and_erase_split_in_ui', async ({ page }) => {
  await page.goto('/');
  const board = page.getByRole('region', { name: 'Resource board' });
  const inbox = board.getByRole('region', { name: 'Inbox', exact: true });
  const draft = inbox.getByRole('textbox', { name: 'Add card to Inbox' });

  await draft.fill('Archive me');
  await draft.press('Enter');
  await expect(inbox.getByRole('button', { name: 'Rename Archive me', exact: true })).toBeVisible();
  await draft.fill('Erase me');
  await draft.press('Enter');
  await expect(inbox.getByRole('button', { name: 'Rename Erase me', exact: true })).toBeVisible();

  // Archive retires the card; delivery history stays preserved.
  page.once('dialog', dialog => dialog.accept());
  await inbox.getByRole('button', { name: 'Archive Archive me' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Card archived.' })).toBeVisible();
  await expect(inbox.getByRole('button', { name: /Archive me/ })).toHaveCount(0);

  // Delete erases the clean card outright.
  page.once('dialog', dialog => dialog.accept());
  await inbox.getByRole('button', { name: 'Delete Erase me' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Card erased.' })).toBeVisible();
  await expect(inbox.getByRole('button', { name: /Erase me/ })).toHaveCount(0);

  // The picker holds neither card.
  await board.getByRole('button', { name: 'Find resource' }).click();
  const picker = page.getByRole('dialog', { name: 'Find resource' });
  await expect(picker.getByText('Archive me')).toHaveCount(0);
  await expect(picker.getByText('Erase me')).toHaveCount(0);
  await picker.getByRole('button', { name: 'Close' }).click();
});
