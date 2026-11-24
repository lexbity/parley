import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties, partiesRegion } from './parties';

test.use({ coachOnline: true });

test('ar02_party_questions_open_relevant_work', async ({ page }, testInfo) => {
  await page.goto('/');
  await openParties(page);
  const region = partiesRegion(page);
  await addParty(page, 'AR02 Scoped Coach Party');
  await expect(region.getByText('AR02 Scoped Coach Party', { exact: true }).first()).toBeVisible();

  const panel = page.getByRole('region', { name: 'Party sheet, neighborhood and inspector' }).getByRole('region', { name: 'Coach questions' });
  await expect(panel).toBeVisible();
  const starts: Array<{ body: unknown; key: string | undefined }> = [];
  page.on('request', request => {
    if (new URL(request.url()).pathname === '/api/v1/coach/context' && request.method() === 'POST') {
      starts.push({ body: request.postDataJSON(), key: request.headers()['idempotency-key'] });
    }
  });
  await expect.poll(() => starts.length).toBe(0);
  const response = page.waitForResponse(item => new URL(item.url()).pathname === '/api/v1/coach/context' && item.request().method() === 'POST');
  await panel.getByRole('button', { name: 'Find questions for this selection' }).click();
  const received = await response;
  expect(starts).toHaveLength(1);
  expect(starts[0]?.key).toBeTruthy();
  expect(starts[0]?.body).toMatchObject({ scope: { kind: 'party' } });
  if (testInfo.project.name === 'rust') {
    expect(received.status()).toBe(503);
    await expect(panel.getByRole('alert')).toContainText('Settings');
  } else {
    expect(received.status()).toBe(202);
    await expect(panel.getByRole('status')).toContainText('Questions queued');
    await expect(panel.getByText(/Which part of this selected context/)).toBeVisible();
    await panel.getByRole('textbox', { name: 'Your answer' }).fill('I want to clarify the current research direction.');
    const answer = page.waitForResponse(item => new URL(item.url()).pathname.includes('/coach/questions/') && item.request().method() === 'POST');
    await panel.getByRole('button', { name: 'Record answer' }).click();
    expect((await answer).status()).toBe(200);
    await expect(panel.getByRole('status')).toContainText('Answer recorded');
  }
});

test('ar02_switching_scope_keeps_the_admitted_context', async ({ page }, testInfo) => {
  await page.goto('/');
  await openParties(page);
  const region = partiesRegion(page);
  await addParty(page, 'AR02 First Scoped Party');
  await addParty(page, 'AR02 Second Scoped Party');
  const panel = region.getByRole('region', { name: 'Coach questions' });
  await expect(panel).toBeVisible();
  await region.getByRole('button', { name: 'AR02 First Scoped Party' }).click();
  await expect(panel.getByText('For AR02 First Scoped Party · party research.')).toBeVisible();
  let releaseRequest!: () => void;
  const held = new Promise<void>(resolve => { releaseRequest = resolve; });
  let captured: { scope: { id: string; revision: number; kind: string } } | undefined;
  await page.route('**/api/v1/coach/context', async route => {
    captured = route.request().postDataJSON() as typeof captured;
    await held;
    await route.continue();
  });
  const response = page.waitForResponse(value => new URL(value.url()).pathname === '/api/v1/coach/context' && value.request().method() === 'POST');
  await panel.getByRole('button', { name: 'Find questions for this selection' }).click();
  await expect.poll(() => captured?.scope.id).toBeTruthy();
  const admittedId = captured?.scope.id;
  await region.getByRole('button', { name: 'AR02 Second Scoped Party' }).click();
  await expect(panel.getByText('For AR02 Second Scoped Party · party research.')).toBeVisible();
  releaseRequest();
  const received = await response;
  expect(captured?.scope.kind).toBe('party');
  expect(captured?.scope.id).toBe(admittedId);
  expect(Number.isSafeInteger(captured?.scope.revision)).toBe(true);
  expect(received.status()).toBe(testInfo.project.name === 'rust' ? 503 : 202);
});
