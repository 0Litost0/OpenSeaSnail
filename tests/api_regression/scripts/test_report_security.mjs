// Real native Playwright report leakage checks, excluded from business acceptance.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {randomUUID,randomBytes} from 'node:crypto';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {project} from '../cli/identity.mjs';
const exec=promisify(execFile);
const example=JSON.parse(await fs.readFile(path.join(project,'contracts/examples/valid/result-passed.json'),'utf8'));
for(const mode of ['assertion','header','attachment','log','source']) {
  test(`native ${mode} canary prevents publication and raw report is destroyed`,async()=>{
    const root=path.join(project,'artifacts/restricted/security-checks',randomUUID());const raw=path.join(root,'raw');const destination=path.join(root,'published');
    await fs.mkdir(raw,{recursive:true,mode:0o700});
    const secret='API_TEST_REPORT_CANARY_'+randomBytes(16).toString('hex');
    const injection=mode==='assertion'?"expect(process.env.REPORT_CANARY).toBe('public');":mode==='header'?"await info.attach('request-header',{body:Buffer.from(JSON.stringify({authorization:'Bearer '+process.env.REPORT_CANARY})),contentType:'application/json'});expect(1).toBe(2);":mode==='attachment'?"await info.attach('binary-content',{body:Buffer.from(process.env.REPORT_CANARY!),contentType:'text/plain'});expect(1).toBe(2);":mode==='log'?"console.log(process.env.REPORT_CANARY);expect(1).toBe(2);":`const sourceOnly=${JSON.stringify(secret)}; expect(1).toBe(2);`;
    await fs.writeFile(path.join(raw,'native.spec.ts'),`import {test,expect} from ${JSON.stringify(path.join(project,'node_modules/@playwright/test/index.mjs'))};\ntest('native security self-check',async({},info)=>{${injection}});\n`,{mode:0o600});
    await fs.writeFile(path.join(raw,'playwright.config.ts'),`import {defineConfig} from ${JSON.stringify(path.join(project,'node_modules/@playwright/test/index.mjs'))};export default defineConfig({testDir:'.',testMatch:'native.spec.ts',workers:1,retries:0,use:{trace:'off'},reporter:[['json',{outputFile:'${raw}/playwright.json'}],['html',{outputFolder:'${raw}/html',open:'never'}]]});`,{mode:0o600});
    try{
      let status=0;try{await exec(process.execPath,[path.join(project,'node_modules/@playwright/test/cli.js'),'test','--config',path.join(raw,'playwright.config.ts')],{cwd:project,env:{...process.env,REPORT_CANARY:secret},timeout:30000,maxBuffer:262144})}catch(error){status=error.code}
      assert.equal(status,1,'native assertion must fail');
      const native=await fs.readFile(path.join(raw,'playwright.json'),'utf8');
      // Header/attachment values are base64 in native JSON; source case checks actual error context.
      const nativeValues=[];
      function inspect(value){if(typeof value==='string')nativeValues.push(value);else if(Array.isArray(value))value.forEach(inspect);else if(value&&typeof value==='object'){if(value.body)nativeValues.push(Buffer.from(value.body,'base64').toString());Object.values(value).forEach(inspect)}}
      inspect(JSON.parse(native));
      assert.ok(nativeValues.some(value=>value.includes(secret)),'canary must enter native report');
      await fs.writeFile(path.join(raw,'secrets.ndjson'),JSON.stringify(secret)+'\n',{mode:0o600});
      await fs.writeFile(path.join(raw,'result.json'),JSON.stringify(example),{mode:0o600});
      let publication;try{publication=await exec('python3',[path.join(project,'scripts/publish_report.py'),raw,destination],{timeout:5000,maxBuffer:8192})}catch(error){publication=error}
      assert.equal(publication.code,1);assert.ok(!publication.stdout.includes(secret));
      await assert.rejects(fs.stat(raw),{code:'ENOENT'});await assert.rejects(fs.stat(destination),{code:'ENOENT'});
    }finally{await fs.rm(root,{recursive:true,force:true})}
  });
}
