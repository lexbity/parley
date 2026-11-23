import { randomUUID } from 'node:crypto';
import AxeBuilder from '@axe-core/playwright';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { createScenario, openPalette, palette, signIn } from './sheet';

test('u01_palette_keyboard_change_and_undo_phase', async ({ page }) => {
  await signIn(page);
  const title = `Palette phase ${randomUUID()}`;
  const id = await createScenario(page, title);
  await page.locator(`tr[data-opportunity-id="${id}"] button.open`).click();
  await expect(page.getByRole('complementary', { name: 'Scenario details' })).toContainText(title);

  // Keyboard only: open, search, move the active option, commit a prompt.
  await openPalette(page);
  const combobox = palette(page).getByRole('combobox');
  await combobox.fill('phase');
  await combobox.press('ArrowDown');
  await combobox.press('ArrowUp');
  const active = await combobox.getAttribute('aria-activedescendant');
  expect(active).toBeTruthy();
  await expect(palette(page).locator(`[id="${active}"]`)).toHaveAttribute('aria-selected', 'true');
  await combobox.press('Enter');
  await expect(palette(page)).toContainText('Planning phase');
  await palette(page).getByLabel('Planning phase').selectOption('preparing');
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await expect(palette(page)).toContainText('Move the selected opportunity to this planning phase?');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(page.getByText('moved to Preparing')).toBeVisible();
  await palette(page).getByRole('button', { name: 'Done' }).click();

  // Undo is also a keyboard palette action with no prompts.
  await openPalette(page);
  await palette(page).getByRole('combobox').fill('undo');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(page.getByText('returned to Unmapped')).toBeVisible();
  await expect(page.getByRole('combobox', { name: `Phase for ${title}`, exact: true })).toHaveValue('');
});

test('u01_palette_accessibility_and_focus', async ({ page }) => {
  await signIn(page);
  await page.getByRole('link', { name: 'Queue' }).click();
  const trigger = page.getByRole('button', { name: 'New opportunity' });
  await trigger.focus();
  await openPalette(page);
  const dialog = palette(page);
  await expect(dialog).toHaveAttribute('aria-labelledby', 'command-palette-title');
  const combobox = dialog.getByRole('combobox');
  await expect(combobox).toHaveAttribute('aria-expanded', 'true');
  const controls = await combobox.getAttribute('aria-controls');
  expect(controls).toBeTruthy();
  await expect(dialog.locator(`[id="${controls}"]`)).toHaveAttribute('role', 'listbox');
  expect(await dialog.getByRole('option').count()).toBeGreaterThan(0);
  const before = await combobox.getAttribute('aria-activedescendant');
  await combobox.press('ArrowDown');
  expect(await combobox.getAttribute('aria-activedescendant')).not.toBe(before);

  // IME composition is neutral: no option movement while a candidate is open.
  await combobox.evaluate((element) => {
    element.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
  });
  const duringIme = await combobox.getAttribute('aria-activedescendant');
  await combobox.press('ArrowDown');
  expect(await combobox.getAttribute('aria-activedescendant')).toBe(duringIme);
  await combobox.evaluate((element) => {
    element.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
  });

  // Descriptor text is rendered as text, never as HTML.
  expect(await dialog.locator('script').count()).toBe(0);
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'])
    .analyze();
  const blocking = results.violations.filter((violation) => ['serious', 'critical'].includes(violation.impact ?? ''));
  expect(blocking, blocking.map((item) => item.id).join(', ')).toEqual([]);

  await combobox.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(trigger).toBeFocused();

  // A removed trigger falls back to the active workspace heading.
  await trigger.evaluate((element) => element.remove());
  await openPalette(page);
  await palette(page).getByRole('combobox').press('Escape');
  await expect(page.getByRole('heading', { name: 'Queue', exact: true })).toBeFocused();
});

test('u01_stale_selection_conflict', async ({ page }) => {
  const session = await signIn(page);
  const title = `Stale phase ${randomUUID()}`;
  const id = await createScenario(page, title);
  await page.locator(`tr[data-opportunity-id="${id}"] button.open`).click();

  await openPalette(page);
  await palette(page).getByRole('combobox').fill('phase');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByLabel('Planning phase').selectOption('preparing');
  await palette(page).getByRole('button', { name: 'Continue' }).click();

  // The authoritative record advances after the intent froze its revision.
  const changed = await page.request.patch(`/api/v1/opportunities/${id}/phase`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { target_stage_id: 'in_conversation', pack_version: '1.0.0', expected_phase_revision: 1 },
  });
  expect(changed.ok()).toBe(true);
  await page.evaluate(() => globalThis.dispatchEvent(new Event('focus')));

  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  const alert = palette(page).getByRole('alert');
  await expect(alert).toBeVisible();
  await expect(alert).toContainText(/revision|stale|version|changed/i);
  await palette(page).getByRole('button', { name: 'Close command palette' }).click();
  await page.evaluate(() => globalThis.dispatchEvent(new Event('focus')));
  await expect(page.getByRole('combobox', { name: `Phase for ${title}`, exact: true })).toHaveValue('in_conversation');
});
