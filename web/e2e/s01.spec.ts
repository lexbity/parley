import { expect, test } from '@playwright/test';
import { launchMock, launchRust, openOwnerPage, stopLaunchedServices } from './launch';

test.afterEach(async () => {
  await stopLaunchedServices();
});

test('s01_resource_first_launch_retry_and_cross_session_conflict', async ({ browser }, testInfo) => {
  const links = testInfo.project.name === 'mock' ? await launchMock() : await launchRust();
  const first = await browser.newPage();
  const second = await browser.newPage();

  await openOwnerPage(first, links[0]);
  await first.getByRole('button', { name: 'Add resource' }).focus();
  await first.keyboard.press('Enter');
  await first.getByLabel('Name').pressSequentially('Onboarding checklist');
  await first.getByLabel('What is it?').pressSequentially('A short review guide for onboarding teams.');
  await first.getByRole('button', { name: 'Save draft' }).focus();
  await first.keyboard.press('Enter');
  await expect(first.getByRole('status')).toContainText('Draft saved to this workspace.');

  await first.reload();
  await expect(first.getByRole('button', { name: /Onboarding checklist/ })).toBeVisible();
  await openOwnerPage(second, links[1]);
  await second.getByRole('button', { name: /Onboarding checklist/ }).click();
  await second.getByLabel('Name').fill('Shared onboarding checklist');
  await second.getByRole('button', { name: 'Save draft' }).click();
  await expect(second.getByRole('status')).toContainText('Draft saved to this workspace.');

  await first.getByRole('button', { name: /Onboarding checklist/ }).click();
  await first.getByLabel('Name').fill('My onboarding checklist');
  await first.getByRole('button', { name: 'Save draft' }).click();
  await expect(first.getByRole('alert')).toContainText('Review the conflict');
  await expect(first.getByRole('alert')).toContainText('Shared onboarding checklist');
  await expect(first.getByRole('alert')).toContainText('My onboarding checklist');
  expect(await first.evaluate(() => Object.keys(localStorage))).toEqual([]);
  expect(await second.evaluate(() => Object.keys(localStorage))).toEqual([]);
});
