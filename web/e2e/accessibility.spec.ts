import AxeBuilder from '@axe-core/playwright';
import { expect } from '@playwright/test';
import { test } from './fixtures';

const routes = [
  ['/resources', 'Resources'],
  ['/leads', 'Parties'],
  ['/queue', 'Queue'],
  ['/output', 'Output'],
  ['/settings', 'Settings'],
] as const;

for (const [path, title] of routes) {
  for (const width of [320, 768, 1440]) {
    test(`F-NFR-2 axe serious and critical findings: ${title} at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(path);
      await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
      const result = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'])
        .analyze();
      const blocking = result.violations.filter((violation) => ['serious', 'critical'].includes(violation.impact ?? ''));
      expect(blocking, blocking.map((item) => `${item.id}: ${item.help}\n${item.nodes.map((node) => node.target.join(', ')).join('\n')}`).join('\n\n')).toEqual([]);
    });
  }
}
