import fs from 'node:fs/promises';
import path from 'node:path';
export function diagnostic(category,code,message,step_id='runner') {return {category,step_id,code,message,expected:null,actual:null,evidence_refs:[]}}
export function failureSummary(result) {
  const summary=structuredClone(result);
  const safeError=error=>diagnostic(error.category,'validation_failed','validation failed');
  summary.errors=summary.errors.map(safeError);
  for(const item of summary.cases)for(const attempt of item.attempts){
    attempt.steps=attempt.steps.map((step,index)=>({...step,title:`Step ${index}`}));
    attempt.errors=attempt.errors.map(safeError);attempt.evidence_refs=[];
  }
  return summary;
}
export function aggregate(result) {
  let incomplete=result.missing_case_ids.length>0||result.report_status==='not_published';let failed=result.report_status==='failed';
  const errors=[...result.errors];
  for(const item of result.cases){failed ||= item.flaky;for(const attempt of item.attempts){incomplete ||= ['not_run','environment_error','interrupted'].includes(attempt.execution_status)||attempt.teardown_status==='unknown';failed ||= attempt.execution_status==='failed'||attempt.teardown_status==='failed';errors.push(...attempt.errors)}}
  for(const error of errors){incomplete ||= ['environment','configuration','interrupted'].includes(error.category);failed ||= !['environment','configuration','interrupted'].includes(error.category)}
  result.gate=incomplete?'incomplete':failed?'failed':'passed';result.exit_code=incomplete?2:failed?1:0;return result;
}
export async function adapt({raw,runId,build,framework,budgetVersion,catalog,required,selected,mode,configurations,runnerStatus}) {
  let native;try{native=JSON.parse(await fs.readFile(path.join(raw,'playwright.json'),'utf8'))}catch{}
  const observed=new Map();const errors=[];
  function walk(suite){for(const spec of suite.specs??[]){const id=spec.title.match(/^([A-Z]+-\d{3})\b/)?.[1];if(!id||observed.has(id)){errors.push(diagnostic('configuration','discovery_result_invalid','native result contains duplicate or unknown identity'));continue}observed.set(id,spec.tests.flatMap(test=>test.results))}for(const child of suite.suites??[])walk(child)}
  for(const suite of native?.suites??[])walk(suite);
  if(!native||runnerStatus.signal)errors.push(diagnostic('interrupted','runner_interrupted','runner ended without a complete framework result'));
  if(runnerStatus.code !== 0 && native && !native.errors?.length && ![...observed.values()].flat().some(attempt=>['failed','timedOut','interrupted'].includes(attempt.status)))errors.push(diagnostic('environment','runner_exit_unexplained','runner exited nonzero without a recorded failure'));
  if(native?.errors?.length)errors.push(diagnostic('configuration','framework_error','framework reported a configuration/dependency error'));
  if([...observed.keys()].some(id=>!selected.some(entry=>entry.case_id===id)))errors.push(diagnostic('configuration','selection_result_mismatch','native result is outside declared selection'));
  const cases=[];
  for(const entry of selected){
    const results=observed.get(entry.case_id)??[];const attempts=[];
    for(let index=0;index<Math.max(1,results.length);index++){
      const nativeAttempt=results[index];let owned;
      try{owned=JSON.parse(await fs.readFile(path.join(raw,'attempts',`${entry.case_id}-${index}.json`),'utf8'))}catch{}
      if(owned && nativeAttempt){
        if(nativeAttempt.status==='interrupted'){owned.execution_status='interrupted';owned.errors.unshift(diagnostic('interrupted','attempt_interrupted','native attempt was interrupted'))}
        if(nativeAttempt.status==='skipped'){owned.execution_status='not_run';if(!owned.errors.some(error=>error.code==='case_not_run'))owned.errors.unshift(diagnostic('configuration','case_not_run','native attempt was skipped'))}
        if(nativeAttempt?.status==='failed'||nativeAttempt?.status==='timedOut'){
          if(owned.execution_status==='passed'&&owned.teardown_status!=='failed')owned.execution_status='failed';
          // A failed framework status also represents setup/teardown failures.
          // Preserve the owner's classification instead of inventing a business failure.
          if(owned.execution_status==='failed'&&!owned.errors.some(error=>['business','timeout','teardown'].includes(error.category)))owned.errors.unshift(diagnostic(nativeAttempt.status==='timedOut'?'timeout':'business','business_validation_failed','business validation failed'));
        }
        attempts.push(owned);continue;
      }
      const status=!native?'interrupted':!nativeAttempt||nativeAttempt.status==='skipped'?'not_run':nativeAttempt.status==='interrupted'?'interrupted':'environment_error';
      attempts.push({attempt_id:`${entry.case_id}-attempt-${index}`,retry_index:index,execution_status:status,teardown_status:status==='not_run'?'not_needed':'unknown',duration_ms:Math.max(0,Math.round(nativeAttempt?.duration??0)),config:configurations[entry.case_id],steps:[],errors:[diagnostic(status==='not_run'?'configuration':status==='interrupted'?'interrupted':'environment',status==='not_run'?'case_not_run':'attempt_result_missing',status==='not_run'?'case filtered, skipped or not implemented':'attempt ownership/result was not completed')],evidence_refs:[]});
    }
    cases.push({case_id:entry.case_id,flaky:attempts.at(-1).execution_status==='passed'&&attempts.slice(0,-1).some(attempt=>attempt.execution_status==='failed'),attempts});
  }
  const actual=cases.filter(item=>item.attempts.some(attempt=>attempt.execution_status!=='not_run')).map(item=>item.case_id);
  return aggregate({schema_version:1,run_id:runId,build,platform:{os:process.platform,arch:process.arch},framework,budget_version:budgetVersion,catalog_version:catalog.catalog_version,required_case_ids:required,missing_case_ids:mode==='acceptance'?required.filter(id=>!actual.includes(id)):[],selection:{mode:mode==='replay'?'run':mode,requested_case_ids:selected.map(entry=>entry.case_id),actual_case_ids:actual,suite_ids:mode==='suite'?[selected[0].suite]:[]},cases,errors,report_status:'not_published',gate:'incomplete',exit_code:2});
}
