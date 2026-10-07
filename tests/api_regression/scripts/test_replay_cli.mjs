// Infrastructure checks only: skipped business placeholders remain incomplete.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {project} from '../cli/identity.mjs';
const exec=promisify(execFile);
async function cli(args){
  try{const result=await exec(process.execPath,[path.join(project,'cli/api-test.mjs'),...args],{cwd:project,timeout:180000});return {code:0,data:JSON.parse(result.stdout)}}
  catch(error){return {code:error.code,data:JSON.parse(error.stdout||error.stderr)}}
}
const source=await cli(['run','CLEAN-002']);assert.equal(source.code,0);assert.equal(source.data.gate,'passed');
const report=path.join(project,path.dirname(source.data.result));
const replay=path.join(report,'replay/CLEAN-002.json');
const configuration=JSON.parse(await fs.readFile(replay,'utf8'));
const reproduced=await cli(['replay',replay]);assert.equal(reproduced.code,0);assert.ok(reproduced.data.result,JSON.stringify(reproduced.data));
const next=path.join(project,path.dirname(reproduced.data.result));
const proof=file=>fs.readFile(path.join(file,'environment-proof/CLEAN-002-0.json'),'utf8').then(JSON.parse);
const [a,b]=await Promise.all([proof(report),proof(next)]);
assert.notEqual(a.home_identity_sha256,b.home_identity_sha256);
assert.notEqual(a.run_id,b.run_id);assert.equal(a.cleanup_status,'passed');assert.equal(b.cleanup_status,'passed');
for(const key of ['runtime','provider','keychain'])assert.deepEqual(a[key],b[key]);
assert.equal(JSON.parse(await fs.readFile(path.join(next,'comparison.json'),'utf8')).exact_replay,true);
const temp=await fs.mkdtemp(path.join(project,'artifacts/replay-check-'));
try{
  async function negative(name,mutate,expected){const value=structuredClone(configuration);mutate(value);const file=path.join(temp,name+'.json');await fs.writeFile(file,JSON.stringify(value));const result=await cli(['replay',file]);assert.equal(result.code,2);assert.equal(result.data.code,expected);return file}
  await negative('asset',value=>value.assets[0].sha256='0'.repeat(64),'replay_asset_hash_mismatch');
  await negative('assertion',value=>value.assertion_version='unknown','replay_contract_invalid');
  await negative('budget',value=>value.budget_version='unknown','replay_budget_version_mismatch');
  await negative('schema',value=>value.schema_version=2,'replay_contract_invalid');
  const changed=await negative('build',value=>value.build.binary_sha256='0'.repeat(64),'replay_build_binary_sha256_mismatch');
  const repaired=await cli(['run','CLEAN-002','--config',changed]);assert.equal(repaired.code,0);assert.ok(repaired.data.result);
  const comparison=JSON.parse(await fs.readFile(path.join(project,path.dirname(repaired.data.result),'comparison.json'),'utf8'));
  assert.equal(comparison.exact_replay,false);assert.equal(comparison.build_changed,true);
  const asset=path.join(project,'artifacts/restricted/source-assets',configuration.build.source_patch.sha256+'.json');
  await fs.rename(asset,asset+'.hold');
  try{const missing=await cli(['replay',replay]);assert.equal(missing.code,2);assert.equal(missing.data.code,'replay_source_asset_missing')}
  finally{await fs.rename(asset+'.hold',asset)}
  const filtered=await cli(['acceptance','--','--grep','AUTH-001']);assert.equal(filtered.code,2);
  const result=JSON.parse(await fs.readFile(path.join(project,filtered.data.result),'utf8'));
  assert.equal(result.required_case_ids.length,22);assert.equal(result.missing_case_ids.length,21);assert.deepEqual(result.missing_case_ids.filter(id=>id==='AUTH-001'),[]);
  assert.equal(result.cases.length,22);assert.equal(result.report_status,'passed');
  const cleanup=await cli(['cleanup-stale']);assert.equal(cleanup.code,0);
  console.log(JSON.stringify({checks:'replay/new-home/repair/hash/version/missing-source/filtered-acceptance/cleanup',passed:true,source_run:source.data.run_id,replay_run:reproduced.data.run_id,repair_run:repaired.data.run_id,acceptance_run:filtered.data.run_id,business_gate:'passed'}));
}finally{await fs.rm(temp,{recursive:true,force:true})}
