import {test as base} from '@playwright/test';
import {test} from '../fixtures/test.js';
import fs from 'node:fs';
import {IMPLEMENTED_CASE_IDS} from './implemented.mjs';
const catalog=JSON.parse(fs.readFileSync(new URL('../case-catalog.json',import.meta.url),'utf8'));
// Every catalog entry remains discoverable while its business implementation is pending.
// Entries in IMPLEMENTED_CASE_IDS are defined by their dedicated spec files.
for(const entry of catalog.cases) {
  if(IMPLEMENTED_CASE_IDS.has(entry.case_id))continue;
  const title=`${entry.case_id} ${entry.purpose}`;const options={tag:`@case_${entry.case_id}`};
  if(entry.quick) {
    test(title,options,async({scenario})=>{
      // The M3 scaffold exercises attempt/replay lifecycle; no business pass is claimed.
      void scenario;test.skip(true,'roadmap business implementation pending');
    });
  } else {
    base(title,options,async()=>{base.skip(true,'roadmap implementation pending');});
  }
}
