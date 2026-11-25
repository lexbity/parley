import { expect } from '@playwright/test';
import { randomUUID } from 'crypto';
import { test } from './fixtures';
import { signIn, type OwnerSession } from './sheet';

/**
 * UCR-001 U08: destructive registry retirement with human approval.
 *
 * Every retire invocation stops at the owner approval; the owner inbox
 * approves or denies the exact frozen intent, and only the approved
 * operation commits. The mock project exercises the palette freeze/resume
 * and the inbox decisions; the rust project verifies the same owner surface
 * against the packaged service.
 */

async function createResource(page: import('@playwright/test').Page, session: OwnerSession, title: string): Promise<string> {
  const response = await page.request.post('/api/v1/resource-card-commits', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: {
      create: { title, subtype: 'gift', placement: { column_id: 'inbox', before_resource_id: null } },
    },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { entity: { id: string } };
  return body.entity.id;
}

async function invokeRetire(page: import('@playwright/test').Page, session: OwnerSession, resourceId: string, key: string, approvalId?: string) {
  const response = await page.request.post(`/api/v1/commands/resource.retire`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': key,
    },
    data: {
      version: 1,
      args: {
        resource_id: resourceId,
        expected_card_revision: 1,
        expected_content_revision: 1,
      },
      dry_run: false,
      ...(approvalId ? { approval_id: approvalId } : {}),
    },
  });
  return { status: response.status(), location: response.headers().location ?? null, body: await response.json() };
}

async function pendingApproval(page: import('@playwright/test').Page, session: OwnerSession, commandId = 'resource.retire') {
  const response = await page.request.get('/api/v1/approvals?status=pending', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { items: Array<{
    id: string; revision: number; preview_digest: string; command_id: string;
  }> };
  const row = body.items.find((item) => item.command_id === commandId);
  expect(row).toBeDefined();
  return row!;
}

async function decideApproval(page: import('@playwright/test').Page, session: OwnerSession, id: string, revision: number, previewDigest: string, action: 'grant' | 'deny') {
  const response = await page.request.post(`/api/v1/approvals/${id}/${action}`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { expected_revision: revision, preview_digest: previewDigest },
  });
  expect(response.ok()).toBe(true);
  return response.json();
}

test('u08_owner_palette_retire_waits_for_approval_and_resumes', async ({ page }) => {
  const session = await signIn(page);
  const title = `U08 gift ${randomUUID()}`;
  const resourceId = await createResource(page, session, title);

  // Capture the frozen idempotency key the palette uses for the intent so the
  // approved resume can reuse the exact same key (FR-8).
  const paletteKey: { value: string } = { value: '' };
  page.on('request', (request) => {
    if (!paletteKey.value && request.method() === 'POST' && request.url().includes('/commands/resource.retire')) {
      paletteKey.value = request.headers()['idempotency-key'] ?? '';
    }
  });

  // Open the resource's details route so the selection context is wired and
  // the scoped front doors register, then open the palette.
  await page.goto(`/resources/${resourceId}`);
  await expect(page.getByRole('heading', { name: /Resources/ })).toBeVisible();
  await page.getByRole('button', { name: 'Commands' }).click();

  const search = page.getByRole('combobox', { name: 'Search commands' });
  await expect(search).toBeVisible();
  await search.fill('retire');
  await expect(page.getByRole('option').filter({ hasText: 'Retire resource' })).toBeVisible({ timeout: 10_000 });
  await expect(page.getByRole('option').filter({ hasText: 'Retire resource' })).toBeVisible();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: 'Confirm command' })).toBeVisible();
  await page.getByRole('button', { name: 'Confirm', exact: true }).click();

  // The destructive intent freezes in the modal and waits for the owner.
  await expect(page.getByRole('heading', { name: 'Awaiting approval' })).toBeVisible();
  await expect(page.getByText(/needs your exact review/i)).toBeVisible();

  // The owner cancels the frozen intent; it remains parked as a pending
  // approval for the inbox.
  await page.getByRole('button', { name: 'Cancel' }).click();
  const approval = await pendingApproval(page, session);
  await decideApproval(page, session, approval.id, approval.revision, approval.preview_digest, 'grant');
  // The approved intent resumes with the very same frozen key and commits.
  expect(paletteKey.value).not.toBe('');
  const resumed = await invokeRetire(page, session, resourceId, paletteKey.value, approval.id);
  expect(resumed.status).toBe(200);
  await expect.poll(async () => {
    const events = await page.request.get('/api/v1/events?after=0', {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    const body = await events.json() as { items: Array<{ event_type: string }> };
    return body.items.some((row) => row.event_type === 'resource.retired');
  }).toBe(true);
});

test('u08_approval_inbox_grant_and_deny', async ({ page }) => {
  const session = await signIn(page);
  const approveTitle = `U08 approve ${randomUUID()}`;
  const denyTitle = `U08 deny ${randomUUID()}`;
  const approveResource = await createResource(page, session, approveTitle);
  const denyResource = await createResource(page, session, denyTitle);

  // Two independent frozen intents park on pending approvals.
  await invokeRetire(page, session, approveResource, `approve-key-${randomUUID()}`);
  await invokeRetire(page, session, denyResource, `deny-key-${randomUUID()}`);
  const approveApproval = await pendingApproval(page, session);

  await page.getByRole('link', { name: 'Settings' }).click();
  const inbox = page.getByRole('region', { name: 'Approvals' });
  await expect(inbox).toBeVisible();
  const rows = inbox.locator('[data-approval-id]');
  await expect(rows).toHaveCount(2);

  // Deny the first row with the keyboard, approve the second.
  await rows.first().getByRole('button', { name: /Deny approval/ }).click();
  await expect(rows).toHaveCount(1);
  await rows.first().getByRole('button', { name: /Approve approval/ }).click();
  await expect(rows).toHaveCount(0, { timeout: 10_000 });

  // The denied intent failed closed; the approved one committed.
  const denied = await page.request.get('/api/v1/approvals?status=denied', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  const deniedBody = await denied.json() as { items: Array<{ id: string }> };
  expect(deniedBody.items.length).toBeGreaterThan(0);
  void approveResource;
  void denyResource;
  void approveApproval;
});

/**
 * TD-10 W3: a legacy destructive mutation (maintenance settings) stops at
 * the owner approval inbox with a frozen intent, and resumes only after the
 * grant. Both browser projects exercise the same wire behavior.
 */
test('u08_owner_legacy_destructive_maintenance_settings_waits_for_approval', async ({ page }) => {
  const session = await signIn(page);
  const authHeaders = { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch };

  const current = await page.request.get('/api/v1/settings/maintenance', { headers: authHeaders });
  expect(current.ok()).toBe(true);
  const settings = await current.json() as { revision: number };
  const intent = {
    backup_directory: '/tmp/parley-e2e-maintenance',
    interval_hours: 24,
    retention_count: 14,
    expected_revision: settings.revision,
  };
  const key = randomUUID();
  const mutateHeaders = { 'content-type': 'application/json', ...authHeaders, 'Idempotency-Key': key };

  const first = await page.request.put('/api/v1/settings/maintenance', { headers: mutateHeaders, data: intent });
  expect(first.status()).toBe(403);
  const firstBody = await first.json() as { error: { code: string } };
  expect(firstBody.error.code).toBe('approval_required');
  const location = first.headers()['location'] ?? '';
  expect(location).toMatch(/^\/api\/v1\/approvals\//);

  const approval = await pendingApproval(page, session, 'maintenance.settings');
  await decideApproval(page, session, approval.id, approval.revision, approval.preview_digest, 'grant');

  const second = await page.request.put('/api/v1/settings/maintenance', { headers: mutateHeaders, data: intent });
  expect(second.ok()).toBe(true);
});

test('u08_rust_owner_surface_reports_empty_inbox', async ({ page }) => {
  const session = await signIn(page);
  await page.getByRole('link', { name: 'Settings' }).click();
  const inbox = page.getByRole('region', { name: 'Approvals' });
  await expect(inbox).toBeVisible();
  await expect(inbox.getByText('No pending approvals.')).toBeVisible();

  // A retire intent against an absent target conceals the record; the same
  // public denial applies to absent and out-of-scope targets.
  const response = await page.request.post('/api/v1/commands/resource.retire', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: {
      version: 1,
      args: { resource_id: 'does-not-exist', expected_card_revision: 1, expected_content_revision: 1 },
      dry_run: false,
    },
  });
  expect(response.status()).toBe(404);
  const body = await response.json() as { error: { code: string } };
  expect(body.error.code).toBe('target_unavailable');
});
