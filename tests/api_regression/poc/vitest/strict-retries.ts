import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
export default class StrictRetries {
    onTestRunEnd(modules) {
        const flaky = modules.flatMap(module => [...module.children.allTests()])
            .filter(test => test.diagnostic()?.flaky).map(test => test.fullName);
        const output = process.env.POC_OUTPUT ?? 'artifacts/vitest';
        mkdirSync(output, { recursive: true });
        writeFileSync(join(output, 'acceptance.json'), JSON.stringify({ flaky, accepted: flaky.length === 0 }, null, 2));
        if (flaky.length) {
            process.exitCode = 1;
            console.error('PoC acceptance: retried failure cannot count as a clean pass');
        }
    }
}
