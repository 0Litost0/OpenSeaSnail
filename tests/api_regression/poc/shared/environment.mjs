import { spawn, execFile } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import { promisify } from 'node:util';
export const password = process.env.POC_PASSWORD ?? randomUUID();
export const providerSecret = process.env.POC_PROVIDER_SECRET ?? randomUUID();
export const delay = ms => new Promise(r => setTimeout(r, ms));
export class Environment {
    constructor() { this.events = []; this.secrets = [password, providerSecret]; }
    async start() {
        this.home ??= await mkdtemp(join(tmpdir(), 'seasnail-api-poc-'));
        const binary = process.env.POC_HOST ?? resolve('../../../target/debug/seasnail-poc-host');
        this.child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, POC_DATA_DIR: this.home, POC_RUNTIME: process.env.POC_RUNTIME ?? 'mock' } });
        const child = this.child;
        this.exited = new Promise(resolve => { child.once('exit', (code, signal) => resolve({ code, signal })); child.once('error', () => resolve({ code: -1, signal: null })); });
        // Bounded diagnostics, sanitized before adding to any error/report.
        let stderr = '';
        child.stderr.on('data', chunk => { stderr = (stderr + chunk.toString()).slice(-4000); });
        const lines = createInterface({ input: child.stdout });
        try {
            const ready = await new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error('environment: host readiness deadline')), 100000);
                const settle = callback => value => { clearTimeout(timer); callback(value); };
                child.once('error', settle(() => reject(new Error('environment: host spawn failed'))));
                child.once('exit', settle(() => reject(new Error(`environment: host exited before ready; ${this.sanitize(stderr)}`))));
                lines.once('line', settle(line => { try {
                    resolve(JSON.parse(line));
                }
                catch {
                    reject(new Error('environment: invalid ready record'));
                } }));
            });
            this.base = `http://127.0.0.1:${ready.port}/api/v1`;
            this.events.push({ event: 'ready', pid: ready.pid, runtime: ready.runtime });
            this.sidecars = [];
            if (ready.runtime === 'sherpa') {
                // Explicit macOS PoC observation adapter; do not claim Windows validation.
                if (process.platform !== 'darwin')
                    throw new Error('environment: sidecar observation needs a platform adapter');
                const { stdout } = await promisify(execFile)('ps', ['-axo', 'pid=,ppid=,comm='], { timeout: 3000 });
                this.sidecars = stdout.split('\n').map(line => line.match(/^\s*(\d+)\s+(\d+)\s+(.+)$/)).filter(m => m && Number(m[2]) === ready.pid && m[3].endsWith('/seasnail-sherpa-sidecar')).map(m => Number(m[1]));
                this.events.push({ event: 'sidecars', pids: this.sidecars });
                if (this.sidecars.length !== 1)
                    throw new Error('environment: expected exactly one owned Sherpa sidecar');
            }
        }
        catch (error) {
            this.events.push({ event: 'startup-error', message: this.sanitize(error.message) });
            try {
                await this.dispose();
            }
            catch (cleanup) {
                this.events.push({ event: 'cleanup-error', message: cleanup.message });
            }
            throw error;
        }
        finally {
            lines.close();
            child.stdout.resume();
        }
    }
    async stop() {
        if (!this.child)
            return;
        const child = this.child;
        child.stdin.end();
        let timer;
        let result = await Promise.race([this.exited, new Promise(resolve => { timer = setTimeout(() => resolve(null), 10000); })]);
        clearTimeout(timer);
        if (!result) {
            child.kill('SIGKILL');
            result = await this.exited;
            this.child = null;
            this.events.push({ event: 'forced-stop', pid: child.pid });
            throw new Error('cleanup: forced host termination');
        }
        this.events.push({ event: 'stopped', pid: child.pid, ...result });
        this.child = null;
        for (const pid of this.sidecars ?? []) {
            const alive = () => { try {
                process.kill(pid, 0);
                return true;
            }
            catch {
                return false;
            } };
            const deadline = Date.now() + 3000;
            while (alive() && Date.now() < deadline)
                await delay(50);
            if (alive())
                throw new Error('cleanup: owned Sherpa sidecar remains alive');
            this.events.push({ event: 'sidecar-exited', pid });
        }
        if (result.code !== 0)
            throw new Error('cleanup: host exited abnormally');
    }
    async restart() { await this.stop(); await this.start(); }
    async dispose() {
        try {
            await this.stop();
        }
        finally {
            if (this.home) {
                await rm(this.home, { recursive: true, force: true });
                this.events.push({ event: 'directory-removed' });
                this.home = null;
            }
        }
    }
    sanitize(value) {
        let output = JSON.stringify(value);
        for (const secret of this.secrets)
            output = output.replaceAll(secret, '[REDACTED]');
        return JSON.parse(output.replace(/ss_live_[A-Za-z0-9_-]+/g, '[REDACTED_TOKEN]'));
    }
}
export async function fetchJson(env, method, path, data, expected = 200) {
    const headers = {};
    if (env.token)
        headers.Authorization = `Bearer ${env.token}`;
    if (data !== undefined)
        headers['Content-Type'] = 'application/json';
    const response = await fetch(`${env.base}${path}`, { method, headers, body: data === undefined ? undefined : JSON.stringify(data), signal: AbortSignal.timeout(5000) });
    if (response.status !== expected)
        throw new Error(`HTTP ${method} ${path}: expected ${expected}, actual ${response.status}`);
    return expected === 204 ? null : response.json();
}
