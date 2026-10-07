import {expect,type APIRequestContext,request} from '@playwright/test';
import {randomBytes} from 'node:crypto';
import fs from 'node:fs/promises';
import {AttemptEnvironment} from './environment.mjs';

const budget=JSON.parse(await fs.readFile(new URL('../assets/budgets.v1.json',import.meta.url),'utf8')).milliseconds;
export class Api {
  token:string|undefined;
  private contexts:APIRequestContext[]=[];
  private pollDeadline:number|undefined;
  readonly password=`synthetic-password-${randomBytes(18).toString('hex')}`;
  readonly providerSecret=`synthetic-provider-${randomBytes(18).toString('hex')}`;
  constructor(readonly env:AttemptEnvironment,private context:APIRequestContext,private secretsFile:string) {this.contexts.push(context)}
  async remember(value:string) {await fs.appendFile(this.secretsFile,JSON.stringify(value)+'\n',{mode:0o600})}
  static async create(env:AttemptEnvironment,secretsFile:string) {
    const context=await request.newContext({timeout:budget.api_request,ignoreHTTPSErrors:false});
    const api=new Api(env,context,secretsFile);await api.remember(api.password);await api.remember(api.providerSecret);return api;
  }
  async response(method:string,route:string,options:{data?:unknown,token?:string|null,multipart?:Record<string,unknown>}={}) {
    const token=options.token===undefined?this.token:options.token;
    return this.context.fetch(this.env.baseURL+route,{method,timeout:Math.max(1,Math.min(budget.api_request,(this.pollDeadline??Infinity)-Date.now())),headers:token?{Authorization:`Bearer ${token}`}:{},...(options.data===undefined?{}:{data:options.data}),...(options.multipart?{multipart:options.multipart as any}:{})});
  }
  async json(method:string,route:string,data?:unknown,status=200,token?:string|null):Promise<any> {
    const response=await this.response(method,route,{data,token});
    expect(response.status(),`${method} ${route} status`).toBe(status);
    if(status===204)return null;return response.json();
  }
  async setup(username='fixture-a') {const account=await this.json('POST','/auth/setup',{username,password:this.password},201,null);this.token=account.secret;await this.remember(account.secret);return account}
  async createAccount(username='fixture-b',password=this.password) {await this.remember(password);const account=await this.json('POST','/accounts',{username,password},201);this.token=account.secret;await this.remember(account.secret);return account}
  async unlock(accountId:string,password=this.password,status=200) {await this.remember(password);const result=await this.json('POST',`/accounts/${accountId}/unlock`,{password},status,null);if(status===200){this.token=result.secret;await this.remember(result.secret)}return result}
  async restart(keychain='persistent') {await this.env.restart({keychain});await this.env.waitModelReady()}
  async configureProvider(endpoint:string,enabled=true,overrides:{name?:string,providerType?:string,model?:string,credential?:string}={}) {
    const provider=await this.json('POST','/reasoning/provider-configs',{name:overrides.name??'synthetic-provider',provider_type:overrides.providerType??'openai_compatible_self_hosted_private',endpoint,model:overrides.model??'synthetic-model'},201);
    await this.json('PUT',`/internal/reasoning/provider-configs/${provider.id}/credential`,{mode:'credential',credential:overrides.credential??this.providerSecret});
    await this.json('PUT','/cleanup/settings',{enabled,selected_provider_config_id:provider.id,custom_prompt:null});return provider;
  }
  async upload(source='imported',language='zh') {
    const buffer=await fs.readFile(new URL('../assets/audio/pipeline-v1.wav',import.meta.url));
    const response=await this.response('POST','/sessions',{multipart:{source,language,audio:{name:'pipeline-v1.wav',mimeType:'audio/wav',buffer}}});
    expect(response.status(),'multipart session status').toBe(202);return (await response.json()).id as string;
  }
  async poll<T>(operation:()=>Promise<T>,done:(value:T)=>boolean,timeout=budget.business_case):Promise<T> {
    const deadline=Date.now()+timeout;
    this.pollDeadline=deadline;
    try {while(Date.now()<deadline){const value=await operation();if(done(value))return value;await new Promise(resolve=>setTimeout(resolve,Math.min(40,Math.max(1,deadline-Date.now()))))}
    // Disposing the context cancels any requests that survive their individual deadline.
    void this.context.dispose().catch(()=>{});throw new Error('business polling deadline');
    } finally {this.pollDeadline=undefined}
  }
  async terminal(id:string,status='completed') {const result=await this.poll(()=>this.json('GET',`/sessions/${id}`),value=>['completed','failed'].includes(value.status));expect(result.status).toBe(status);return result}
  async binary(route:string,status=200) {const response=await this.response('GET',route);expect(response.status()).toBe(status);return response.body()}
  async close() {await Promise.all(this.contexts.map(context=>context.dispose()))}
}
