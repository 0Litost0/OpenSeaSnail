import { test, expect } from 'vitest';
import { Environment, fetchJson } from '../shared/environment.mjs';
import { dictionaryScenario } from '../shared/scenario.mjs';
test('DICT-001 dictionary CRUD and process restart', async () => {
    const env = new Environment();
    try {
        await env.start();
        await dictionaryScenario(env, (...args) => fetchJson(env, ...args), async (name, fn) => {
            console.log(`STEP ${name}`);
            try {
                await fn();
            }
            catch (error) {
                error.message = `${name}: ${error.message}`;
                throw error;
            }
        }, (a, b) => expect(a).toEqual(b), process.env.POC_FAULT === '1');
    }
    finally {
        await env.dispose();
        console.log(JSON.stringify(env.sanitize(env.events)));
    }
});
