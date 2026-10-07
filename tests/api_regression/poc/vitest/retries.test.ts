import { test, expect } from 'vitest';
import { Environment, fetchJson } from '../shared/environment.mjs';
import { dictionaryScenario } from '../shared/scenario.mjs';
let attempt = 0;
test('FLAKY-001 preserve first failure and retry', { retry: 1 }, async (context) => {
    const env = new Environment();
    const record = { attempt: attempt++, steps: [], status: 'running', lifecycle: [] };
    const attempts = context.task.meta.attempts ??= [];
    attempts.push(record);
    try {
        await env.start();
        await dictionaryScenario(env, (...args) => fetchJson(env, ...args), async (name, fn) => {
            const step = { name, status: 'running' };
            record.steps.push(step);
            try {
                await fn();
                step.status = 'passed';
            }
            catch (error) {
                step.status = 'failed';
                throw error;
            }
        }, (a, b) => expect(a).toEqual(b), record.attempt === 0);
        record.status = 'passed';
    }
    catch (error) {
        record.status = 'failed';
        throw error;
    }
    finally {
        try {
            await env.dispose();
        }
        finally {
            record.lifecycle = env.sanitize(env.events);
        }
    }
});
