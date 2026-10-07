import { test as base } from '@playwright/test';
import { Environment } from '../shared/environment.mjs';
export const test = base.extend<{
    env: Environment;
}>({
    env: async ({}, use, info) => {
        const env = new Environment();
        try {
            await env.start();
            await use(env);
        }
        finally {
            try {
                await env.dispose();
            }
            finally {
                await info.attach('lifecycle', { body: JSON.stringify(env.sanitize(env.events), null, 2), contentType: 'application/json' });
            }
        }
    },
});
