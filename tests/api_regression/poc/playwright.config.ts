import { defineConfig } from '@playwright/test';
export default defineConfig({
    testDir: './playwright', workers: 1, timeout: 150000, globalTimeout: 360000,
    retries: 0, forbidOnly: true, failOnFlakyTests: true,
    outputDir: process.env.POC_OUTPUT ? `${process.env.POC_OUTPUT}/test-results` : 'artifacts/playwright/test-results',
    reporter: [['list'], ['json', { outputFile: `${process.env.POC_OUTPUT ?? 'artifacts/playwright'}/results.json` }], ['html', { outputFolder: `${process.env.POC_OUTPUT ?? 'artifacts/playwright'}/html`, open: 'never' }]],
    use: { trace: 'off' },
});
