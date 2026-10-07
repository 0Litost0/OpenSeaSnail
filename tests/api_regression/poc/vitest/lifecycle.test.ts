import { test, expect } from 'vitest';
import { Environment } from '../shared/environment.mjs';
test('LIFE-002 unexpected host death is a cleanup error', async (context) => {
    const env = new Environment();
    context.onTestFinished(async () => { try {
        await env.dispose();
    }
    finally {
        context.task.meta.lifecycle = env.sanitize(env.events);
    } });
    await env.start();
    env.child.kill('SIGKILL');
    await env.exited;
    await expect(env.stop()).rejects.toThrow('host exited abnormally');
});
if (process.env.POC_TIMEOUT === '1')
    test('LIFE-timeout controlled test deadline', { timeout: 1500 }, async (context) => {
        const env = new Environment();
        context.onTestFinished(async () => { try {
            await env.dispose();
        }
        finally {
            context.task.meta.lifecycle = env.sanitize(env.events);
        } });
        await env.start();
        await new Promise((_, reject) => {
            if (context.signal.aborted)
                reject(new Error('test aborted'));
            else
                context.signal.addEventListener('abort', () => reject(new Error('test aborted')), { once: true });
        });
    });
