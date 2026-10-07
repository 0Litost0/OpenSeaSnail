import { expect } from '@playwright/test';
import { test } from './fixtures';
import { dictionaryScenario } from '../shared/scenario.mjs';
test('DICT-001 dictionary CRUD and process restart', async ({ env, playwright }, info) => {
    const client = await playwright.request.newContext({ timeout: 5000 });
    try {
        const call = async (method, path, data, expected = 200) => {
            const response = await client.fetch(`${env.base}${path}`, { method, data, headers: env.token ? { Authorization: `Bearer ${env.token}` } : {} });
            expect(response.status(), `HTTP ${method} ${path}`).toBe(expected);
            return expected === 204 ? null : response.json();
        };
        const fault = process.env.POC_FAULT === '1' || (process.env.POC_FLAKY === '1' && info.retry === 0);
        await dictionaryScenario(env, call, (name, fn) => test.step(name, fn), (a, b) => expect(a).toEqual(b), fault);
    }
    finally {
        await client.dispose();
    }
});
