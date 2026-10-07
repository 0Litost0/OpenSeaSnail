// Internal fixture verification, excluded from the 23 registered business cases.
import {test,expect} from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {AttemptEnvironment} from '../fixtures/environment.mjs';
import {Api} from '../fixtures/api.js';
import {localProvider} from '../fixtures/provider.mjs';
for(const mode of ['success','http-503','invalid-json','timeout','interrupted']) {
  test(`fixture self-check ${mode}`,async()=>{
    const root=await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'seasnail-api-fixture-')));
    const env=new AttemptEnvironment({root,caseId:'CLEAN-001'});let api:Api|undefined;let provider:Awaited<ReturnType<typeof localProvider>>|undefined;
    try{
      await env.prepare();await env.start();await env.waitModelReady();
      api=await Api.create(env,path.join(root,'secrets.ndjson'));provider=await localProvider(mode,api.providerSecret);
      await test.step('API setup and authenticated provider configuration',async()=>{await api!.setup();await api!.json('POST','/dictionary/entries',{terms:['SeaSnail']});await api!.configureProvider(provider!.endpoint)});
      const id=await test.step('multipart upload and bounded task polling',async()=>api!.upload('realtime'));
      await api.terminal(id);const detail=await api.json('GET',`/sessions/${id}/workspace-detail`);
      expect(detail.cleanup_status).toBe(mode==='success'?'succeeded':'failed');
      expect(detail.final_text).toBe(mode==='success'?'海螺支持语音转写和词典功能。':'海螺支持语音转写和词典功能');
      expect(provider.requests).toHaveLength(1);expect(provider.requests[0].authenticated).toBe(true);expect(provider.requests[0].input.dictionary_terms).toContain('SeaSnail');
      if(mode==='timeout')expect(detail.cleanup_error_code).toBe('cleanup_timeout');
      const bytes=await api.binary(`/sessions/${id}/audio`);expect(bytes).toEqual(await fs.readFile(new URL('../assets/audio/pipeline-v1.wav',import.meta.url)));
      await api.restart();expect((await api.json('GET',`/sessions/${id}/workspace-detail`)).final_text).toBe(detail.final_text);
    }finally{
      await api?.close();const cleanup=await env.teardown();await provider?.close();expect(cleanup.status).toBe('passed');if(cleanup.status==='passed')await fs.rm(root,{recursive:true});
    }
  });
}
