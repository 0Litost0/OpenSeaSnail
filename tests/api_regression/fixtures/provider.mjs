import http from 'node:http';
import fs from 'node:fs/promises';
const scenes=JSON.parse(await fs.readFile(new URL('../assets/scenarios/provider.json',import.meta.url),'utf8')).scenarios;
export async function localProvider(mode,secret) {
  if(!Object.hasOwn(scenes,mode))throw new Error('unknown provider scenario');
  const requests=[];const sockets=new Set();
  const server=http.createServer(async(req,res)=>{
    try{
      if(req.method!=='POST'||req.url!=='/v1/chat/completions'){res.writeHead(404);res.end();return}
      let body='';for await(const chunk of req){body+=chunk;if(body.length>1024*1024){req.destroy();return}}
      const payload=JSON.parse(body);const user=payload.messages?.find(message=>message.role==='user')?.content;
      const input=typeof user==='string'?JSON.parse(user):{};
      requests.push({authenticated:req.headers.authorization===`Bearer ${secret}`,input:{transcript:input.transcript??input.original_text??input.text,dictionary_terms:input.dictionary_terms??[]},model:payload.model});
      if(mode==='timeout')return;
      if(mode==='interrupted'){res.destroy();return}
      const scenario=scenes[mode];
      res.writeHead(scenario.http_status??503,{'Content-Type':'application/json'});
      if(mode==='http-503'||mode==='disabled'){res.end(JSON.stringify({error:{message:'controlled failure'}}));return}
      const content=typeof scenario.content==='string'?scenario.content:JSON.stringify(scenario.content);
      res.end(JSON.stringify({choices:[{finish_reason:'stop',message:{role:'assistant',content}}]}));
    }catch{res.writeHead(400);res.end(JSON.stringify({error:{message:'fixture invalid input'}}))}
  });
  server.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket))});
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve)});
  const {port}=server.address();
  return {endpoint:`http://127.0.0.1:${port}/v1`,requests,async close(){for(const socket of sockets)socket.destroy();await new Promise((resolve,reject)=>{server.close(error=>error?reject(error):resolve())})}};
}
