import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { openOwnerPage, test } from './fixtures';

test('s01_resource_first_launch_retry_and_cross_session_conflict', async ({ page, browser, ownerSession }, testInfo) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  const title = `Onboarding checklist ${randomUUID()}`;
  const board = page.getByRole('region', { name: 'Resource board', exact: true });
  const editor = page.getByRole('form', { name: 'Resource card editor' });
  await board.getByRole('button', { name: 'Add card to Inbox' }).click();
  await editor.getByLabel('Title').fill(title);
  await editor.getByRole('textbox', { name: 'Owner notes' }).fill('A short **review guide** for onboarding teams.');
  await editor.getByRole('button', { name: 'Save card' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Card saved.' })).toBeVisible();

  await page.reload();
  const edit = board.getByRole('button', { name: `Edit ${title}`, exact: true });
  await expect(edit).toBeVisible();
  if (testInfo.project.name === 'rust') {
    // The packaged binary serves the embedded Vite bundle from hashed assets.
    const script = page.locator('script[type="module"]');
    await expect(script).toHaveAttribute('src', /^\/assets\//);
    const scriptUrl = await script.evaluate((element: HTMLScriptElement) => element.src);
    await expect(await page.request.get(scriptUrl)).toBeOK();
  }

  await edit.click();
  await editor.getByLabel('Title').fill(`Mine ${title}`);
  const second = await browser.newPage();
  try {
    await openOwnerPage(second, await ownerSession.secondBootstrapUrl());
    await second.getByRole('button', { name: `Edit ${title}`, exact: true }).click();
    const secondEditor = second.getByRole('form', { name: 'Resource card editor' });
    await expect(secondEditor.getByRole('textbox', { name: 'Owner notes' })).toContainText('review guide');
    await secondEditor.getByLabel('Title').fill(`Shared ${title}`);
    await secondEditor.getByRole('button', { name: 'Save card' }).click();
    await expect(second.getByRole('status').filter({ hasText: 'Card saved.' })).toBeVisible();

    await editor.getByRole('button', { name: 'Save card' }).click();
    const conflict = editor.getByRole('alert').filter({ hasText: 'Conflict: this card changed in another session' });
    await expect(conflict).toContainText(`Shared ${title}`);
    await expect(conflict).toContainText(`Mine ${title}`);
    expect(await page.evaluate(() => Object.keys(localStorage))).toEqual([]);
    expect(await second.evaluate(() => Object.keys(localStorage))).toEqual([]);
  } finally {
    await second.close();
  }
});
