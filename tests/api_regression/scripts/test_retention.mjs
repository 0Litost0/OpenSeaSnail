import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {pruneEvidence} from '../cli/retention.mjs';
test('seven-day retention removes only owned published reports',async()=>{
  const project=await fs.mkdtemp(path.join(os.tmpdir(),'seasnail-retention-'));
  try{
    await fs.mkdir(path.join(project,'assets'));await fs.copyFile(new URL('../assets/budgets.v1.json',import.meta.url),path.join(project,'assets/budgets.v1.json'));
    const root=path.join(project,'artifacts/reports');await fs.mkdir(root,{recursive:true});
    const old='11111111-1111-1111-1111-111111111111',fresh='22222222-2222-2222-2222-222222222222';
    for(const id of [old,fresh,'unowned']){await fs.mkdir(path.join(root,id));await fs.writeFile(path.join(root,id,'result.json'),JSON.stringify({run_id:id,schema_version:1}))}
    const past=new Date(Date.now()-8*86400000);await fs.utimes(path.join(root,old),past,past);await fs.utimes(path.join(root,'unowned'),past,past);
    assert.deepEqual(await pruneEvidence(project),[old]);assert.deepEqual((await fs.readdir(root)).sort(),[fresh,'unowned'].sort());
  }finally{await fs.rm(project,{recursive:true,force:true})}
});
