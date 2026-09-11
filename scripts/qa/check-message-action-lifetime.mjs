import fs from 'node:fs';
import ts from 'typescript';
import assert from 'node:assert/strict';
const source=fs.readFileSync('src/App.vue','utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
const ast=ts.createSourceFile('App.ts',source,ts.ScriptTarget.ES2022,true);
const names=['handleSysMessageRead','handleAllSysMessagesRead','handleSysMessageView'];
const functions=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast)).join('\n');
assert.equal(ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).length,3);
const js=ts.transpileModule(functions,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.None}}).outputText;
const deferred=()=>{let resolve,reject;const promise=new Promise((r,j)=>{resolve=r;reject=j});return {promise,resolve,reject}};
const message=id=>({id,dedupeKey:id});
function harness(){
 const pending={value:''},all={value:false},current={value:message('a')},error={value:''},queue={value:[]},calls=[],reads=new Map(),opens=new Map();
 const batch=deferred();
 const context={sysMessageReadPendingKey:pending,sysMessageReadAllPending:all,currentSysMessage:current,sysMessageActionError:error,sysMessageQueue:queue,
  isCurrentSysMessageReadPending:{get value(){return all.value||current.value?.dedupeKey===pending.value}},
  isSysMessagePreview:false,sysMessageEnrichmentGeneration:1,
  sysMessageService:{markRead(m){calls.push(m.id);const d=deferred();reads.set(m.id,d);return d.promise},markAllRead(){return batch.promise}},
  hideCurrentSysMessage(m){if(current.value?.id===m.id)current.value=null},expireStaleSysMessages(){},hidePanelWindow(){},deliverTasksWhenSystemMessagesFinish(){},
  openSysMessageDetail(m){const d=deferred();opens.set(m.id,d);return d.promise},storage:{setLastSysMessageDetail(){}},console:{warn(){}}
 };
 const actions=new Function(...Object.keys(context),js+`\nreturn {${names.join(',')}}`)(...Object.values(context));
 return {...actions,pending,all,current,error,queue,calls,reads,opens,batch};
}
{
 const h=harness(), a=h.current.value;
 const old=h.handleSysMessageRead(a);
 h.current.value=message('b');
 const next=h.handleSysMessageRead(h.current.value);
 assert.deepEqual(h.calls,['a','b']);
 h.reads.get('a').reject(new Error('old failure'));await old;
 assert.equal(h.error.value,'');assert.equal(h.pending.value,'b');
 h.reads.get('b').resolve();await next;assert.equal(h.current.value,null);assert.equal(h.pending.value,'');
}
{
 const h=harness(),a=h.current.value;
 const old=h.handleSysMessageView(a);h.current.value=message('b');
 h.opens.get('a').resolve(false);await old;
 assert.equal(h.error.value,'');assert.equal(h.current.value.id,'b');
}
{
 const h=harness();h.queue.value=[message('a2')];const old=h.handleAllSysMessagesRead();
 h.current.value=message('b');h.queue.value=[];h.batch.reject(new Error('old batch failure'));await old;
 assert.equal(h.error.value,'');assert.equal(h.current.value.id,'b');assert.equal(h.all.value,false);
}
{
 const h=harness();const current=h.handleSysMessageRead(h.current.value);h.reads.get('a').reject(new Error('current failure'));await current;
 assert.match(h.error.value,/未能标记已读/);assert.equal(h.current.value.id,'a');assert.equal(h.pending.value,'');
}
const result={passed:true,source:'actual App.vue action declarations, isolated with deferred services',cases:['expired old request does not block new card action','old read error does not overwrite new feedback or pending state','old detail-open result does not attach to next card','old read-all failure does not attach to a message outside snapshot','current request failure still reports and restores controls']};
fs.mkdirSync('artifacts/message-action-qa',{recursive:true});
fs.writeFileSync('artifacts/message-action-qa/report.json',JSON.stringify(result,null,2));console.log(result);
