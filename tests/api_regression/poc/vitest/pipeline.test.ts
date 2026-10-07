import { test, expect } from 'vitest';
import { Environment } from '../shared/environment.mjs';
import { fetchClient } from '../shared/fetch-client.mjs';
import { cleanupScenario, sherpaScenario } from '../shared/pipeline.mjs';
async function run(context, scenario) {
    const env = new Environment();
    const steps = [];
    context.task.meta.businessSteps = steps;
    try {
        await env.start();
        await scenario({ env, client: fetchClient, expect,
            poll: (fn, value, timeout) => expect.poll(fn, { timeout, interval: 250 }).toBe(value),
            attach: async (name, attachment) => { context.task.meta[name] = JSON.parse(attachment.body); await context.annotate(name, { ...attachment, body: Buffer.from(attachment.body) }); },
            step: async (name, fn) => {
                const record = { name, status: 'running' };
                steps.push(record);
                await context.annotate(name, 'business-step');
                try {
                    await fn();
                    record.status = 'passed';
                }
                catch (error) {
                    record.status = 'failed';
                    error.message = `${name}: ${error.message}`;
                    throw error;
                }
            } });
    }
    finally {
        try {
            await env.dispose();
        }
        finally {
            context.task.meta.lifecycle = env.sanitize(env.events);
        }
    }
}
for (const mode of ['success', 'error', 'invalid', 'timeout'])
    test(`CLEAN-${mode} multipart, dictionary and fallback`, context => run(context, options => cleanupScenario(options, mode)));
test('SHERPA-001 real runtime and versioned speech', context => run(context, sherpaScenario));
