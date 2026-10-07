import {AttemptEnvironment} from '../fixtures/environment.mjs';
const env=new AttemptEnvironment({root:process.argv[2],caseId:'AUTH-004'});
await env.prepare();await env.start();
console.log(JSON.stringify({home:env.home,pid:env.child.pid,manifest:env.recoveryPath}));
setInterval(()=>{},1000);
