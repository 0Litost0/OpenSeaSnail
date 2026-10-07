import { expect } from '@playwright/test';
import { test } from './fixtures';
import { cleanupScenario, sherpaScenario } from '../shared/pipeline.mjs';
function adapter(env, client, info) {
    return { env, client, expect, step: (name, fn) => test.step(name, fn),
        poll: (fn, value, timeout) => expect.poll(fn, { timeout, intervals: [100, 250, 500] }).toBe(value),
        attach: (name, attachment) => info.attach(name, attachment) };
}
for (const mode of ['success', 'error', 'invalid', 'timeout']) {
    test(`CLEAN-${mode} multipart, dictionary and fallback`, async ({ env, playwright }, info) => {
        const client = await playwright.request.newContext({ timeout: 10000 });
        try {
            await cleanupScenario(adapter(env, client, info), mode);
        }
        finally {
            await client.dispose();
        }
    });
}
test('SHERPA-001 real runtime and versioned speech', async ({ env, playwright }, info) => {
    const client = await playwright.request.newContext({ timeout: 10000 });
    try {
        await sherpaScenario(adapter(env, client, info));
    }
    finally {
        await client.dispose();
    }
});
