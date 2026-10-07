import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {collectDiagnostics,assertionDetails} from '../fixtures/diagnostics.mjs';
test('captures late host logs, exit status and assertion differences with secrets removed',async()=>{
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'api-diagnostics-'));const raw=path.join(root,'raw');const home=path.join(root,'home');
  try{
    await fs.mkdir(raw);await fs.mkdir(path.join(home,'logs'),{recursive:true});
    const secret='synthetic-private-value';await fs.writeFile(path.join(raw,'secrets.ndjson'),JSON.stringify(secret)+'\n');
    await fs.writeFile(path.join(home,'logs','backend.log.2026-10-06'),'late business error '+secret);
    const env={root,home,diagnosticStreams:[{pid:123,stderr:'shutdown '+secret,stdout:'',exit:{code:1,signal:null}}]};
    const evidence=await collectDiagnostics({env,raw,caseId:'CLEAN-002',retry:0,errors:[{message:'assert '+secret}]});
    const text=await fs.readFile(path.join(raw,evidence.reference),'utf8');assert.ok(!text.includes(secret));assert.ok(text.includes('late business error'));assert.ok(text.includes('shutdown'));
    assert.equal(JSON.parse(text).hosts[0].exit.code,1);
    assert.deepEqual(assertionDetails({message:'difference',matcherResult:{expected:'raw',actual:'wrong'}}),{message:'difference',expected:'raw',actual:'wrong'});
    env.diagnosticStreams[0].stderr='x'.repeat(400000);await collectDiagnostics({env,raw,caseId:'CLEAN-002',retry:1});
    const bounded=JSON.parse(await fs.readFile(path.join(raw,'environment-proof/CLEAN-002-1-diagnostics.json'),'utf8'));assert.equal(bounded.hosts[0].stderr.text.length,262144);assert.equal(bounded.hosts[0].stderr.truncated,true);
  }finally{await fs.rm(root,{recursive:true,force:true})}
});
