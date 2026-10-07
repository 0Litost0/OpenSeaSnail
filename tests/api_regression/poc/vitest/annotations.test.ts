// Fairness check: can Vitest's existing annotation/meta APIs close the report gap?
import { test, expect } from 'vitest';
import { Environment, fetchJson } from '../shared/environment.mjs';
import { dictionaryScenario } from '../shared/scenario.mjs';
test('DICT-001 dictionary with Vitest annotations and metadata', async (context) => {
    const env = new Environment();
    const steps = [];
    context.task.meta.businessSteps = steps;
    try {
        await env.start();
        await dictionaryScenario(env, (...args) => fetchJson(env, ...args), async (name, fn) => {
            const step = { name, status: 'running' };
            steps.push(step);
            await context.annotate(name, 'business-step');
            try {
                await fn();
                step.status = 'passed';
            }
            catch (error) {
                step.status = 'failed';
                error.message = `${name}: ${error.message}`;
                throw error;
            }
        }, (a, b) => expect(a).toEqual(b), process.env.POC_FAULT === '1');
    }
    finally {
        try {
            await env.dispose();
        }
        finally {
            context.task.meta.lifecycle = env.sanitize(env.events);
            await context.annotate('lifecycle', { body: Buffer.from(JSON.stringify(env.sanitize(env.events))), contentType: 'application/json' });
        }
    }
});
