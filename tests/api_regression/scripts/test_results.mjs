// Adapter unit fixtures are synthetic; they never count as executed business cases.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {adapt,aggregate,diagnostic,failureSummary} from '../cli/result.mjs';
const example=JSON.parse(await fs.readFile(new URL('../contracts/examples/valid/result-passed.json',import.meta.url),'utf8'));
const catalog=JSON.parse(await fs.readFile(new URL('../case-catalog.json',import.meta.url),'utf8'));
const selected=[catalog.cases.find(entry=>entry.case_id==='CLEAN-002')];
async function fixture(results,owned=[],native=true) {
  const raw=await fs.mkdtemp(path.join(os.tmpdir(),'seasnail-result-unit-'));
  await fs.mkdir(path.join(raw,'attempts'));
  if(native)await fs.writeFile(path.join(raw,'playwright.json'),JSON.stringify({suites:[{specs:[{title:'CLEAN-002 synthetic adapter input',tests:[{results}]}]}]}));
  for(let i=0;i<owned.length;i++)await fs.writeFile(path.join(raw,'attempts',`CLEAN-002-${i}.json`),JSON.stringify(owned[i]));
  try{return await adapt({raw,runId:'synthetic-adapter-unit',build:example.build,framework:example.framework,budgetVersion:example.budget_version,catalog,required:example.required_case_ids,selected,mode:'run',configurations:{'CLEAN-002':example.cases[0].attempts[0].config},runnerStatus:{code:native?0:null,signal:native?null:'SIGKILL'}})}finally{await fs.rm(raw,{recursive:true})}
}
test('business failure and teardown failure both survive adaptation',async()=>{
  const attempt=structuredClone(example.cases[0].attempts[0]);attempt.execution_status='failed';attempt.teardown_status='failed';attempt.steps[0].status='failed';attempt.errors=[diagnostic('business','assertion_failed','synthetic assertion failure'),diagnostic('teardown','cleanup_failed','synthetic cleanup failure')];
  const result=await fixture([{status:'failed',retry:0,duration:1}],[attempt]);result.report_status='passed';aggregate(result);assert.equal(result.exit_code,1);assert.equal(result.cases[0].attempts[0].errors.length,2);
});
test('first failure remains in history when retry passes and gate stays nonzero',async()=>{
  const failed=structuredClone(example.cases[0].attempts[0]);failed.execution_status='failed';failed.steps[0].status='failed';failed.errors=[diagnostic('business','assertion_failed','synthetic assertion failure')];
  const passed=structuredClone(example.cases[0].attempts[0]);passed.retry_index=1;passed.attempt_id='attempt-1';
  const result=await fixture([{status:'failed',retry:0},{status:'passed',retry:1}],[failed,passed]);result.report_status='passed';aggregate(result);assert.equal(result.cases[0].flaky,true);assert.equal(result.cases[0].attempts.length,2);assert.equal(result.exit_code,1);
});
test('skip, missing attempt and interrupted runner are incomplete',async()=>{
  for(const [results,native] of [[[{status:'skipped'}],true],[[{status:'passed'}],true],[[],false]]){
    const result=await fixture(results,[],native);result.report_status='passed';aggregate(result);assert.equal(result.exit_code,2);assert.notEqual(result.cases[0].attempts[0].execution_status,'passed');
  }
});

test('native interruption or skip cannot be overridden by an owned passed attempt',async()=>{
  for(const status of ['interrupted','skipped']){
    const owned=structuredClone(example.cases[0].attempts[0]);
    const result=await fixture([{status,retry:0}],[owned]);result.report_status='passed';aggregate(result);
    assert.equal(result.exit_code,2);assert.equal(result.cases[0].attempts[0].execution_status,status==='skipped'?'not_run':'interrupted');
  }
});

test('failed-publication summary strips assertion values, error text and step titles',()=>{
  const result=structuredClone(example);const attempt=result.cases[0].attempts[0];
  const secret='local-secret-in-structured-result';attempt.steps[0].title=secret;
  attempt.errors=[{...diagnostic('business',secret,secret),expected:secret,actual:secret,evidence_refs:[secret]}];attempt.evidence_refs=[secret];
  result.errors=[diagnostic('report',secret,secret)];
  const summary=failureSummary(result);assert.ok(!JSON.stringify(summary).includes(secret));
  assert.equal(summary.cases[0].attempts[0].errors[0].category,'business');
});


test('setup failure stays environment-only even when the framework reports failed',async()=>{
  const owned=structuredClone(example.cases[0].attempts[0]);owned.execution_status='environment_error';owned.steps=[];
  owned.errors=[diagnostic('environment','environment_setup_failed','setup unavailable')];
  const result=await fixture([{status:'failed',retry:0}],[owned]);result.report_status='passed';aggregate(result);
  assert.equal(result.exit_code,2);assert.deepEqual(result.cases[0].attempts[0].errors.map(error=>error.category),['environment']);
});


test('a cleanup-only framework failure does not rewrite successful business execution',async()=>{
  const owned=structuredClone(example.cases[0].attempts[0]);owned.teardown_status='failed';
  owned.errors=[diagnostic('teardown','cleanup_failed','cleanup unavailable')];
  const result=await fixture([{status:'failed',retry:0}],[owned]);result.report_status='passed';aggregate(result);
  assert.equal(result.exit_code,1);assert.equal(result.cases[0].attempts[0].execution_status,'passed');
  assert.deepEqual(result.cases[0].attempts[0].errors.map(error=>error.category),['teardown']);
});
