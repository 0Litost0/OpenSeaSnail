import { defineConfig } from 'vitest/config';
export default defineConfig({ test: {
        environment: 'node', include: ['vitest/*.test.ts'], fileParallelism: false, maxWorkers: 1,
        testTimeout: 150000, hookTimeout: 20000,
        reporters: ['default', 'json', 'junit'],
        outputFile: { json: `${process.env.POC_OUTPUT ?? 'artifacts/vitest'}/results.json`, junit: `${process.env.POC_OUTPUT ?? 'artifacts/vitest'}/junit.xml` },
    } });
