import { randomUUID } from 'node:crypto';
import { expect, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { signIn, type OwnerSession } from './sheet';

async function grantedParty(page: Page, session: OwnerSession, name: string) {
  await openParties(page);
  await addParty(page, name, 'person');
  const response = await page.request.get('/api/v1/entities?kind=person&kind=org', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { data: Array<{ id: string; name: string }> };
  const party = body.data.find((row) => row.name === name);
  expect(party).toBeDefined();
  return party!;
}

async function createOpportunity(page: Page, session: OwnerSession, primaryId: string, title: string) {
  const response = await page.request.post('/api/v1/opportunities', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { title, primary_party_id: primaryId },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { id?: string; data?: { id: string } };
  const id = body.data?.id ?? body.id;
  expect(typeof id).toBe('string');
  return id!;
}

async function openRuns(page: Page) {
  await page.getByRole('link', { name: 'Settings' }).click();
  return page.getByRole('region', { name: 'Delegated runs' });
}

test('u07_delegated_writing_grants_are_visible_but_concealed_from_the_owner_palette', async ({ page }) => {
  const session = await signIn(page);
  const name = `U07 grant ${randomUUID()}`;
  const party = await grantedParty(page, session, name);
  const title = `U07 opp ${randomUUID()}`;
  const opportunityId = await createOpportunity(page, session, party.id, title);

  const runs = await openRuns(page);
  await expect(runs.getByRole('checkbox', { name: `Party: ${name}` })).toBeVisible();
  await runs.getByRole('checkbox', { name: 'Grant match.start' }).check();
  await runs.getByRole('checkbox', { name: 'Grant material.compose' }).check();
  await runs.getByRole('checkbox', { name: 'Grant material.gate' }).check();
  await runs.getByRole('checkbox', { name: `Opportunity: ${title}` }).check();
  await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();

  // Agent-only writing commands are concealed from the owner surface: describe
  // returns the same unknown_command denial, and the palette manifest omits
  // them entirely.
  for (const id of ['match.start', 'material.compose_draft', 'material.check_gate']) {
    const response = await page.request.get(`/api/v1/commands/${encodeURIComponent(id)}`, {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    expect(response.status()).toBe(404);
    const body = await response.json() as { error: { code: string } };
    expect(body.error.code).toBe('unknown_command');
  }
  const query = await page.request.post('/api/v1/commands/query', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
    },
    data: { surface: 'palette', context: { route: 'queue', 'selection.opportunity': true } },
  });
  expect(query.ok()).toBe(true);
  const manifest = await query.json() as { commands: Array<{ id: string }> };
  const ids = manifest.commands.map((command) => command.id);
  expect(ids).not.toContain('match.start');
  expect(ids).not.toContain('material.compose_draft');
  expect(ids).not.toContain('material.check_gate');

  // The opportunity exists so the grant target checkbox reflects it.
  expect(opportunityId.length).toBeGreaterThan(0);
});

test('u07_delegated_writing_run_reports_waiting_and_opens_output_review', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U07 writing ${randomUUID()}`;
  const party = await grantedParty(page, session, name);
  const title = `U07 opp ${randomUUID()}`;
  const opportunityId = await createOpportunity(page, session, party.id, title);

  const runs = await openRuns(page);
  await runs.getByRole('textbox', { name: 'Task for delegated run' }).fill(`Compose a draft for ${title}`);
  await runs.getByRole('checkbox', { name: `Opportunity: ${title}` }).check();
  await runs.getByRole('checkbox', { name: 'Grant material.compose' }).check();
  await runs.getByRole('checkbox', { name: 'Grant job.read' }).check();
  await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();
  await runs.getByRole('button', { name: 'Start scoped run' }).click();
  const row = runs.locator('[data-run-id]').first();
  await expect(row).toBeVisible();
  const id = (await row.getAttribute('data-run-id'))!;
  await expect(runs.getByRole('status')).toContainText(`Run ${id} started`);

  if (testInfo.project.name === 'rust') {
    // The packaged service has no loopback provider here, so the writing job
    // cannot succeed; the run must still be observed in a durable state with
    // the command pipeline reachable.
    await expect(row).toHaveAttribute('data-state', /^(queued|running|waiting_job|failed)$/, { timeout: 30_000 });
    return;
  }

  // The writing run parks on its background job, then succeeds with a review
  // reference scoped to the opportunity.
  await expect(row).toHaveAttribute('data-state', 'waiting_job', { timeout: 15_000 });
  await expect(runs.getByText(/Waiting for its background job/i)).toBeVisible();
  await expect(row).toHaveAttribute('data-state', 'succeeded', { timeout: 30_000 });

  const statusResponse = await page.request.get(`/api/v1/agent/runs/${id}`, {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(statusResponse.ok()).toBe(true);
  const status = await statusResponse.json() as {
    state: string;
    review: { job_id: string; scope_kind: string; scope_id: string } | null;
  };
  expect(status.state).toBe('succeeded');
  expect(status.review).toEqual({ job_id: expect.any(String), scope_kind: 'opportunity', scope_id: opportunityId });

  // The review opens the Output workspace on that opportunity's record.
  const reviewButton = runs.getByRole('button', { name: `Open reviewed candidate for run ${id}` });
  await expect(reviewButton).toBeVisible();
  await reviewButton.click();
  await expect(page).toHaveURL(new RegExp(`/output/${opportunityId}$`));
  const workspace = page.getByRole('region', { name: 'Output workspace' });
  await expect(workspace.getByRole('combobox', { name: 'Opportunity', exact: true })).toHaveValue(opportunityId);
});
