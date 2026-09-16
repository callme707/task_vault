// Isolated UI preview. Only explicit UI files and synthetic in-memory records are exposed.
// This process cannot access TaskVault's settings, credentials, databases, or attachments.
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const ui = path.resolve(__dirname, '../ui');
const fixture = String.raw`
(() => {
  const listeners = new Map();
  const fire = (name) => (listeners.get(name) || []).forEach(fn => fn({payload:null}));
  const stamp = '2026-09-16T02:00:00.000Z';
  let next = 20;
  const task = (id, title, status, priority, tags, description, extra={}) => ({id,title,status,priority,tags,description,kind:'task',due_at:null,planned_start:null,planned_end:null,estimate_minutes:60,actual_minutes:0,progress:0,source_inbox_id:null,source_snapshot:null,is_archived:false,deleted_at:null,created_at:stamp,updated_at:stamp,subtasks:[],...extra});
  const tasks = [
    task(1,'整理本周产品评审反馈','in_progress','P1',['产品设计','本周重点'],'汇总交互走查中的关键问题，确认下一轮迭代的优先级。',{progress:45,due_at:'2026-09-18T10:00:00Z',planned_start:'2026-09-16T01:00:00Z',planned_end:'2026-09-16T02:00:00Z',subtasks:[{id:1,task_id:1,text:'归纳记录区反馈',is_done:true,position:0},{id:2,task_id:1,text:'准备评审清单',is_done:false,position:1}]}),
    task(2,'完成任务工作台的交互细节','todo','P1',['TaskVault','开发'],'检查任务筛选、详情编辑和键盘操作，让记录与整理更顺手。',{due_at:'2026-09-19T10:00:00Z'}),
    task(3,'准备周五的项目同步材料','todo','P2',['协作'],'整理本周进展、待确认问题和下周计划。'),
    task(4,'核对图片识别出的待办','blocked','P2',['记录区'],'等待补充会议截图中的截止日期。'),
    task(5,'更新本地开发环境说明','done','P3',['文档'],'补充启动方式与数据保存路径。',{progress:100,actual_minutes:35}),
    task(6,'归档上月的工作记录','done','P3',['归档'],'已完成的历史记录。',{is_archived:true,progress:100})
  ];
  const inbox = [
    {id:3,raw_text:'周五之前把工作台的交互细节走查一遍，重点看任务筛选和快捷录入。',source:'hotkey',created_at:stamp,ai_status:'done',ai_result:JSON.stringify({title:'走查工作台交互细节',kind:'task',description:'检查任务筛选、详情编辑和快捷录入，整理需要调整的问题。',priority:'P1',tags:['设计','工作台'],subtasks:['检查任务筛选','验证快捷录入'],confidence:0.92,summary:'在周五前完成工作台交互走查',items:[]}),ai_model:'演示模型',is_promoted:false,deleted_at:null,image_path:null,item_keys:['text'],promoted_keys:[],ai_generation:1},
    {id:2,raw_text:'下次复盘时讨论一下：记录区能不能更快地筛出需要处理的信息？',source:'hotkey',created_at:stamp,ai_status:'pending',ai_result:null,ai_model:null,is_promoted:false,deleted_at:null,image_path:null,item_keys:['text'],promoted_keys:[],ai_generation:0},
    {id:1,raw_text:'截图里的两项待办需要分别确认。',source:'image',created_at:stamp,ai_status:'done',ai_result:JSON.stringify({summary:'两项待办需要跟进',items:[{title:'整理会议结论',description:'汇总会议记录并确认负责人',priority:'P1',kind:'task',tags:['会议']},{title:'更新验收清单',description:'补充本轮迭代的验收项目',priority:'P2',kind:'task',tags:['项目']}]}),ai_model:'演示视觉模型',is_promoted:true,deleted_at:null,image_path:'fixture.png',item_keys:['image:one','image:two'],promoted_keys:['image:one'],ai_generation:1}
  ];
  let settings={provider:'openai',base_url:'https://api.deepseek.com',model:'deepseek-v4-flash',vision_model:null,disable_reasoning:true,auto_parse:false,autostart:false,quick_hotkey:'Ctrl+Alt+Q',main_hotkey:'Ctrl+Alt+W',has_api_key:true};
  const clone = value => structuredClone(value);
  const invoke=async (command,args={}) => {
    switch(command) {
      case 'list_inbox':return clone(inbox.filter(x=>!x.deleted_at).sort((a,b)=>b.id-a.id));
      case 'list_tasks':return clone(tasks.filter(x=>!x.is_archived&&!x.deleted_at));
      case 'list_archived_tasks':return clone(tasks.filter(x=>x.is_archived&&!x.deleted_at));
      case 'get_settings':return clone(settings);
      case 'save_settings':settings={...args.settings,has_api_key:args.settings.remove_api_key?false:(!!args.settings.api_key||settings.has_api_key)};delete settings.api_key;return clone(settings);
      case 'create_task':{const t=task(next++,args.draft.title,args.draft.status,args.draft.priority,args.draft.tags,args.draft.description,{...args.draft,subtasks:(args.draft.subtasks||[]).map((text,i)=>({id:next++,text,is_done:false,position:i}))});tasks.unshift(t);fire('tasks-updated');return clone(t);}
      case 'update_task':{const t=tasks.find(t=>t.id===args.task.id);Object.assign(t,args.task);fire('tasks-updated');return clone(t);}
      case 'archive_task':case 'unarchive_task':{const t=tasks.find(t=>t.id===args.id);t.is_archived=command==='archive_task';fire('tasks-updated');return clone(t);}
      case 'delete_task':tasks.find(t=>t.id===args.id).deleted_at=stamp;fire('tasks-updated');return;
      case 'set_task_priority':{const t=tasks.find(t=>t.id===args.id);t.priority=args.priority;fire('tasks-updated');return clone(t);}
      case 'promote_inbox':{const item=inbox.find(x=>x.id===args.inboxId);if(item.promoted_keys.includes(args.itemKey))throw new Error('该待办已添加');item.promoted_keys.push(args.itemKey);item.is_promoted=true;const result=await invoke('create_task',{draft:args.draft});fire('inbox-updated');return result;}
      case 'parse_inbox_item':{const item=inbox.find(x=>x.id===args.id);item.ai_status='processing';fire('inbox-updated');setTimeout(()=>{item.ai_status='done';item.ai_result=JSON.stringify({title:item.raw_text.slice(0,26),description:item.raw_text,kind:'task',priority:'P2',tags:['待整理'],items:[]});fire('inbox-updated');},500);return;}
      case 'process_pending_inbox':{const list=inbox.filter(x=>['pending','failed','unconfigured','skipped'].includes(x.ai_status));for(const item of list) await invoke('parse_inbox_item',{id:item.id});return list.length;}
      case 'capture_inbox':case 'capture_inbox_image':{const item={id:next++,raw_text:args.text||'',source:'hotkey',created_at:new Date().toISOString(),ai_status:'skipped',ai_result:null,ai_model:null,is_promoted:false,deleted_at:null,image_path:null,item_keys:['text'],promoted_keys:[],ai_generation:0};inbox.unshift(item);fire('inbox-updated');return clone(item);}
      case 'delete_inbox':inbox.find(x=>x.id===args.id).deleted_at=stamp;fire('inbox-updated');return;
      case 'read_inbox_image':return 'data:image/svg+xml;charset=utf-8,'+encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="640" height="220" viewBox="0 0 640 220"><rect width="640" height="220" rx="12" fill="#f0f2f4"/><g font-family="Segoe UI,Microsoft YaHei,sans-serif" fill="#24343c"><text x="30" y="45" font-size="20" font-weight="600">会议待办 · 合成示例</text><rect x="30" y="80" width="18" height="18" rx="3" fill="none" stroke="#667c86"/><text x="64" y="96" font-size="18">整理会议结论，确认负责人</text><rect x="30" y="133" width="18" height="18" rx="3" fill="none" stroke="#667c86"/><text x="64" y="149" font-size="18">更新验收清单，补充本轮验收项目</text><text x="30" y="193" font-size="12" fill="#667c86">此图片仅用于界面预览，不包含真实记录</text></g></svg>');
      case 'add_subtask':{const t=tasks.find(x=>x.id===args.taskId);const s={id:next++,task_id:t.id,text:args.text,is_done:false,position:t.subtasks.length};t.subtasks.push(s);fire('tasks-updated');return clone(s);}
      case 'set_subtask_done':{const s=tasks.flatMap(t=>t.subtasks).find(s=>s.id===args.id);s.is_done=args.isDone;fire('tasks-updated');return clone(s);}
      case 'delete_subtask':for(const t of tasks)t.subtasks=t.subtasks.filter(s=>s.id!==args.id);fire('tasks-updated');return;
      case 'hide_window':return;
      default:throw new Error('预览未实现命令：'+command);
    }
  };
  window.__TAURI__={core:{invoke},event:{listen:async(name,fn)=>{listeners.set(name,[...(listeners.get(name)||[]),fn]);return()=>{};}},window:{getCurrentWindow:()=>({startDragging:async()=>{}})}};
  window.__TASKVAULT_PREVIEW__=true;
  document.addEventListener('DOMContentLoaded',()=>{document.title+=' · 合成数据预览';});
})();
`;
const types={'.html':'text/html; charset=utf-8','.js':'text/javascript; charset=utf-8','.css':'text/css; charset=utf-8','.png':'image/png','.svg':'image/svg+xml'};
const allowed = new Set([
  'index.html','quick.html','main.js','quick.js','window-controls.js',
  'main.css','quick.css','time.js','app-icon.png',
  'icons/inbox.svg','icons/list-todo.svg','icons/settings-2.svg','icons/search.svg',
  'icons/plus.svg','icons/layers.svg','icons/x.svg','icons/minus.svg',
  'icons/square.svg','icons/copy.svg',
]);
const server=http.createServer((req,res)=>{
  const url=new URL(req.url,'http://127.0.0.1');
  if(url.pathname==='/__fixture.js'){res.writeHead(200,{'Content-Type':types['.js'],'Cache-Control':'no-store'});res.end(fixture);return;}
  const name=url.pathname==='/'?'index.html':url.pathname.slice(1);
  if(!allowed.has(name)){res.writeHead(404);res.end('Not found');return;}
  try {
    let data=fs.readFileSync(path.join(ui,name));
    if(name.endsWith('.html')) data=Buffer.from(data.toString().replace('<head>','<head><script src="/__fixture.js"></script>'));
    res.writeHead(200,{'Content-Type':types[path.extname(name)]||'application/octet-stream','Cache-Control':'no-store','X-Content-Type-Options':'nosniff'});res.end(data);
  }catch {res.writeHead(404);res.end('Not found');}
});
server.listen(Number(process.env.TASKVAULT_PREVIEW_PORT||0),'127.0.0.1',()=>console.log('TaskVault synthetic UI preview: http://127.0.0.1:'+server.address().port));
