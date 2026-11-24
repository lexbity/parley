import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { openPalette, palette, signIn } from './sheet';

test('u06_palette_creates_draft_and_marks_it_ready', async ({ page }) => {
  const session = await signIn(page);
  await page.getByRole('link', { name: 'Resources' }).click();

  // Create a draft entirely from the palette: title prompt + reviewed
  // subtype picker, then confirm.
  await openPalette(page);
  const combobox = palette(page).getByRole('combobox');
  await combobox.fill('Create resource draft');
  await combobox.press('Enter');
  const title = `Palette gift ${randomUUID()}`;
  await palette(page).getByLabel('Title').fill(title);
  await palette(page).getByLabel('Subtype').selectOption('gift');
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(palette(page)).toContainText('completed');
  await palette(page).getByRole('button', { name: 'Done' }).click();

  // The draft is real: it shows up in the resource directory and remains a
  // draft with the typed gaps reported by the readiness command.
  const list = await page.request.get('/api/v1/entities?kind=resource', {
    headers: {
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
    },
  });
  const body = await list.json();
  const found = body.data.find((resource: { title: string }) => resource.title === title);
  expect(found).toBeTruthy();
  expect(found.readiness).toBe('draft');
  const id: string = found.id;

  // Selecting the card (pick it from the off-board picker) makes it the
  // active record so the readiness command is scope-visible.
  await page.getByRole('button', { name: 'Find resource' }).click();
  const picker = page.getByRole('list', { name: 'Resource picker' });
  await picker.getByRole('button', { name: title }).click();
  await expect(page.getByRole('heading', { name: title }).first()).toBeVisible();

  await openPalette(page);
  await palette(page).getByRole('combobox').fill('Mark resource ready');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  // Incomplete gift fields are reported as bounded gaps; zero domain effect.
  await expect(palette(page).getByRole('alert')).toContainText(/missing required fields/i);
  await palette(page).getByRole('button', { name: 'Close command palette' }).click();

  // Complete the typed fields through the shared REST editor (same adapter
  // the editor form uses), then mark ready from the palette again.
  const card = await page.request.get(`/api/v1/entities/${id}`, {
    headers: {
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
    },
  });
  const cardBody = await card.json();
  const etag = cardBody.data.etag as string;
  const edit = await page.request.patch(`/api/v1/entities/${id}`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'If-Match': etag,
      'Idempotency-Key': randomUUID(),
    },
    data: {
      title,
      body: { what: 'Checklist', format: 'template', delivery: 'on_request', time_to_produce_minutes: 20, cost_to_me: 'low' },
    },
  });
  expect(edit.ok()).toBe(true);
  // Reload so the board snapshot and the selected card's content revision
  // reflect the edited fields before the palette freezes its intent.
  await page.reload();

  await openPalette(page);
  await palette(page).getByRole('combobox').fill('Mark resource ready');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(palette(page)).toContainText('completed');
  await palette(page).getByRole('button', { name: 'Done' }).click();

  // The board/inspector reflect the ready transition through the refresh
  // path; the card is eligible and shows no gaps.
  await expect(page.getByRole('status').filter({ hasText: 'Resource is ready.' })).toBeVisible();
  const ready = await page.request.get(`/api/v1/resource-cards/${id}`, {
    headers: {
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
    },
  });
  const readyBody = await ready.json();
  expect(readyBody.data.readiness).toBe('ready');
  expect(readyBody.data.content_revision).toBe(3);
});

test('u06_null_resource_commands_are_route_and_selection_scoped', async ({ page }) => {
  const session = await signIn(page);
  await page.getByRole('link', { name: 'Resources' }).click();
  await openPalette(page);
  const combobox = palette(page).getByRole('combobox');
  await combobox.fill('Mark resource ready');
  // No selected card on a bare resources route: the command stays hidden.
  await expect(palette(page).locator('[role="option"]').filter({ hasText: 'Mark resource ready' })).toHaveCount(0);
  await combobox.fill('Create resource draft');
  // Creation is route-scoped but selection-free; it must be discoverable.
  await expect(palette(page).locator('[role="option"]').filter({ hasText: 'Create resource draft' })).toHaveCount(1);
  await palette(page).getByRole('button', { name: 'Close command palette' }).click();

  // The metrics surface never exposes the agent-only summary command.
  await openPalette(page);
  await palette(page).getByRole('combobox').fill('get_summary');
  await expect(palette(page).locator('[role="option"]').filter({ hasText: 'get_summary' })).toHaveCount(0);
  await palette(page).getByRole('button', { name: 'Close command palette' }).click();

  // Arbitrary palette requests hit the real registry contract.
  const denied = await page.request.post('/api/v1/commands/resource.get_summary', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { version: 1, args: { resource_id: 'res_missing' }, dry_run: false },
  });
  expect(denied.status()).toBe(404);
  const failure = await denied.json();
  expect(failure.error.code).toBe('unknown_command');
});
