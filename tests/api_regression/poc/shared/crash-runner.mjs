// Deliberately has no teardown handler: parent test kills this runner with SIGKILL.
import { Environment } from './environment.mjs';
const env = new Environment();
await env.start();
console.log(JSON.stringify({ pid: env.child.pid, home: env.home, base: env.base, sidecars: env.sidecars ?? [] }));
setInterval(() => { }, 1000);
