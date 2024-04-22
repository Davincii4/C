import { test, expect } from '@playwright/test';

import { percySnapshot } from './helper';

test.describe('Acceptance | 404', { tag: '@acceptance' }, () => {
  test('/unknown-route shows a 404 page', async ({ page }, testInfo) => {
    await page.goto('/unknown-route');
    await expect(page).toHaveURL('/unknown-route');
    await expect(page.locator('[data-test-404-page]')).toBeVisible();
    await expect(page.locator('[data-test-title]')).toHaveText('Page not found');
    await expect(page.locator('[data-test-go-back]')).toBeVisible();
    await expect(page.locator('[data-test-try_again]')).toHaveCount(0);
    await percySnapshot(page, testInfo);
  });
});
