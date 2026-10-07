import { test, expect } from '@playwright/test';
import { test as hostTest } from './fixtures';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { rm, access } from 'node:fs/promises';
import { Environment, delay } from '../shared/environment.mjs';
test('LIFE-001 runner SIGKILL and recovery cleanup', async ({}, info) => {
    const runner = spawn(process.execPath, ['shared/crash-runner.mjs'], { stdio: ['ignore', 'pipe', 'pipe'], env: process.env });
    runner.stderr.resume();
    const exited = new Promise(resolve => runner.once('exit', resolve));
    const lines = createInterface({ input: runner.stdout });
    let record;
    try {
        record = await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error('runner ready deadline')), 110000);
            lines.once('line', line => { clearTimeout(timer); resolve(JSON.parse(line)); });
            runner.once('error', () => { clearTimeout(timer); reject(new Error('runner spawn failed')); });
            runner.once('exit', () => { clearTimeout(timer); reject(new Error('runner exited before ready')); });
        });
        runner.kill('SIGKILL');
        await exited;
        await expect.poll(() => { try {
            process.kill(record.pid, 0);
            return true;
        }
        catch {
            return false;
        } }, { timeout: 15000, intervals: [100, 250] }).toBe(false);
        for (const pid of record.sidecars)
            await expect.poll(() => { try {
                process.kill(pid, 0);
                return true;
            }
            catch {
                return false;
            } }, { timeout: 5000, intervals: [100, 250] }).toBe(false);
        // SIGKILL cannot run JS teardown. Recovery owns the directory from the ready record.
        await rm(record.home, { recursive: true, force: true });
        await expect(access(record.home)).rejects.toThrow();
        await info.attach('recovery', { body: JSON.stringify({ runnerKilled: true, hostExited: true, ownedSidecarsExited: record.sidecars.length, directoryRemoved: true, runtime: process.env.POC_RUNTIME ?? 'mock' }), contentType: 'application/json' });
    }
    finally {
        lines.close();
        runner.kill('SIGKILL');
        await exited;
        if (record) {
            try {
                process.kill(record.pid, 0);
                process.kill(record.pid, 'SIGKILL');
            }
            catch { }
            await rm(record.home, { recursive: true, force: true });
        }
    }
});
test('LIFE-002 unexpected host death is a cleanup error', async () => {
    const env = new Environment();
    try {
        await env.start();
        env.child.kill('SIGKILL');
        await env.exited;
        await expect(env.stop()).rejects.toThrow('host exited abnormally');
    }
    finally {
        await env.dispose();
    }
});
if (process.env.POC_TIMEOUT === '1')
    hostTest('LIFE-timeout controlled test deadline', async ({ env }) => {
        hostTest.setTimeout(1500);
        expect(env.child.pid).toBeGreaterThan(0);
        await delay(10000);
    });
