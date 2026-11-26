(() => {
  'use strict';

  const labels = {
    'inbound.create': '创建入站', 'inbound.expiry': '修改入站有效期',
    'client.create': '添加客户端', 'client.expiry': '修改客户端有效期',
    'outbound.upsert': '配置出站', 'route.upsert': '配置路由', backup: '下载备份'
  };
  const pages = {
    panels: ['面板管理', '已接入面板', '面板', 'server', '接入面板', 'add-panel'],
    inbounds: ['入站管理', '入站列表', '入站', 'log-in', '批量创建入站', 'inbound.create'],
    clients: ['客户端管理', '客户端列表', '客户端', 'users', '批量添加客户端', 'client.create'],
    outbounds: ['出站管理', '出站配置', '出站', 'log-out', '批量配置出站', 'outbound.upsert'],
    routes: ['路由管理', '路由规则', '路由', 'route', '批量配置路由', 'route.upsert'],
    backups: ['数据备份', '备份面板', '面板', 'database', '下载备份', 'backup'],
    activity: ['操作记录', '最近操作', '记录', 'history']
  };
  const esc = v => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;'}[c]));
  const list = v => Array.isArray(v) ? v : [];
  const date = v => Number(v) > 0 ? new Date(Number(v)).toLocaleString('zh-CN',{hour12:false,year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'}) : Number(v) < 0 ? `首次使用后 ${Math.abs(Number(v))/86400000} 天` : '永久有效';
  const badge = (text,tone='info') => `<span class="wf-badge wf-${tone}"><i></i>${esc(text)}</span>`;
  const button = (ctx,text,action,icon,primary=false) => `<button type="button" class="wf-button${primary?' wf-primary':''}" data-wf-open="${esc(action)}">${icon?ctx.icon(icon,16):''}${esc(text)}</button>`;
  const empty = (ctx,text) => `<div class="wf-empty"><span class="wf-empty-icon">${ctx.icon('inbox')}</span><p>${esc(text)}</p></div>`;
  const entity = (ctx,icon,title,meta='') => `<div class="wf-entity"><span class="wf-entity-icon">${ctx.icon(icon)}</span><span><strong>${esc(title)}</strong>${meta?`<small class="wf-meta">${esc(meta)}</small>`:''}</span></div>`;
  const refLink = (action,index,text) => `<button type="button" class="wf-link" data-wf-row="${action}" data-wf-index="${index}">${esc(text)}</button>`;
  const field = (title,control,tip='') => `<label class="wf-field"><span>${esc(title)}</span>${control}${tip?`<small>${esc(tip)}</small>`:''}</label>`;
  const input = (name,placeholder='',value='',attrs='') => `<input name="${name}" placeholder="${esc(placeholder)}" value="${esc(value)}" ${attrs}>`;
  function readSettings(inbound) {
    if (typeof inbound.settings !== 'string') return inbound.settings || {};
    try { return JSON.parse(inbound.settings) || {}; } catch { return {}; }
  }
  async function resources(ctx,panel,kind) {
    const res=await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/${kind}`);
    return kind==='inbounds'?list(res.inbounds):res.config;
  }
  function modal(ctx,title,icon,body,footer='') {
    document.querySelector('dialog.wf-dialog')?.close();
    const el=document.createElement('dialog');
    el.className='wf-dialog';
    el.setAttribute('aria-labelledby','wf-dialog-title');
    el.innerHTML=`<div class="wf-dialog-head"><span class="wf-modal-icon">${ctx.icon(icon)}</span><h2 id="wf-dialog-title">${esc(title)}</h2><button type="button" class="wf-icon-button" data-wf-close aria-label="关闭">${ctx.icon('x')}</button></div><div class="wf-dialog-body">${body}</div>${footer?`<div class="wf-dialog-foot">${footer}</div>`:''}`;
    el.addEventListener('click',ev=>{
      if(el.dataset.busy==='1') return;
      if(ev.target.closest('[data-wf-close]')) el.close();
      if(ev.target===el){
        const r=el.getBoundingClientRect();
        if(ev.clientX<r.left||ev.clientX>r.right||ev.clientY<r.top||ev.clientY>r.bottom) el.close();
      }
    });
    el.addEventListener('cancel',ev=>{if(el.dataset.busy==='1')ev.preventDefault();});
    el.addEventListener('close',()=>el.remove(),{once:true});
    document.body.append(el);
    el.showModal();
    return el;
  }
  function submitFooter(text) {
    return `<button type="button" class="wf-button" data-wf-close>取消</button><button type="submit" form="wf-form" class="wf-button wf-primary" data-wf-submit>${esc(text)}</button>`;
  }
  function bindForm(el,handler) {
    const form=el.querySelector('form');
    form.addEventListener('submit',async ev=>{
      ev.preventDefault();
      if (el.dataset.busy==='1') return;
      const btn=el.querySelector('[data-wf-submit]');
      const original=btn.textContent;
      const err=el.querySelector('.wf-inline-error');
      err.textContent='';
      btn.disabled=true;
      btn.textContent='正在处理…';
      el.dataset.busy='1';
      form.inert=true;
      try {await handler(form);}
      catch(e){err.textContent=e.message||String(e);err.scrollIntoView({block:'nearest'});}
      finally{btn.disabled=false;btn.textContent=original;delete el.dataset.busy;form.inert=false;}
    });
  }
  function panelNoteField(note='') {
    return field('面板备注（可选）',`<textarea name="note" maxlength="2048" rows="3" placeholder="填写面板备注">${esc(note)}</textarea>`);
  }
  function editPanelNote(ctx,panel,draw) {
    const expectedNote=panel.note || '';
    const body=`<form id="wf-form" class="wf-form"><strong>${esc(panel.name)}</strong>${panelNoteField(expectedNote)}<div class="wf-inline-error" role="alert"></div></form>`;
    const el=modal(ctx,'编辑面板备注','pencil',body,submitFooter('保存备注'));
    bindForm(el,async form=>{
      const note=form.elements.note.value.trim();
      const saved=await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/note`,{method:'POST',body:{note,expectedNote}});
      panel.note=saved.note;
      const current=ctx.state.panels.find(p=>p.id===panel.id);
      if(current) current.note=panel.note;
      el.close();
      draw();
      ctx.toast('面板备注已保存');
    });
  }
  function addPanel(ctx) {
    const body=`<form id="wf-form" class="wf-form"><div class="wf-grid">${field('面板名称',input('name','例如 香港主面板','','maxlength="80" autocomplete="off"'),'留空时使用面板域名。')}${field('地区（可选）',input('region','例如 香港','','maxlength="80"'))}</div>${field('面板地址',input('url','https://panel.example.com/自定义路径/','','type="url" required autocomplete="off" spellcheck="false"'),'填写含自定义路径的面板根地址。')}${field('分享地址（可选）',input('publicHost','公网域名或 IP','','autocomplete="off" spellcheck="false"'),'当客户端连接地址与面板域名不同时填写。')}${panelNoteField()}${field('API 令牌',input('token','输入 3x-ui API 令牌','','type="password" required autocomplete="new-password" spellcheck="false"'))}<label class="wf-check"><input type="checkbox" name="sync" checked>接入后同步已有数据</label><div class="wf-inline-error" role="alert"></div></form>`;
    const el=modal(ctx,'接入面板','server',body,submitFooter('测试并接入'));
    bindForm(el,async form=>{
      const fields=Object.fromEntries(new FormData(form));
      fields.name=fields.name.trim()||new URL(fields.url).hostname;
      fields.sync=form.elements.sync.checked;
      const res=await ctx.api('/api/panels',{method:'POST',body:fields});
      try{if(res.panel?.id)localStorage.setItem('nexus.line.panel',res.panel.id);}catch{}
      form.elements.token.value='';
      el.close();
      await ctx.refresh();
      if(res.syncError)showSync(ctx,res.panel,{error:res.syncError,connected:true});
      else if(res.sync)showSync(ctx,res.panel,{result:res.sync,connected:true});
      else ctx.toast('面板已接入');
    });
  }
  function detail(ctx,panel) {
    if(!panel) return ctx.toast('找不到该面板');
    const pairs=[['面板名称',panel.name],['面板备注',panel.note||'—'],['面板地址',panel.url||'—'],['地区',panel.region||'—'],['连接状态',panel.online===true?'在线':panel.online===false?'离线':'待同步'],['入站数量',panel.inbounds??'—'],['客户端数量',panel.clients??'—']];
    const body=`<dl class="wf-detail-list">${pairs.map(([k,v])=>`<div><dt>${esc(k)}</dt><dd>${esc(v)}</dd></div>`).join('')}</dl>`;
    const el=modal(ctx,panel.name,'server',body,`<button type="button" class="wf-button wf-danger-link" data-remove>删除面板</button><button type="button" class="wf-button" data-panel-sync>同步</button><button type="button" class="wf-button wf-primary" data-backup>${ctx.icon('download')}备份面板</button>`);
    el.querySelector('[data-backup]').onclick=()=>backup(ctx,panel.id,true);
    el.querySelector('[data-panel-sync]').onclick=()=>showSync(ctx,panel);
    el.querySelector('[data-remove]').onclick=()=>removePanel(ctx,panel);
  }

  function showSync(ctx,panel,initial={}) {
    if(!panel)return ctx.toast('找不到该面板','error');
    const el=modal(ctx,'同步面板数据','refresh-cw',
      `<b>${esc(panel.name)}</b><div class="wf-sync-status" role="status" aria-live="polite"></div>`,
      '<button type="button" class="wf-button wf-primary" data-wf-close>完成</button>');
    const status=el.querySelector('[role=status]');
    const close=el.querySelector('.wf-dialog-foot [data-wf-close]');
    function display(result,error) {
      status.replaceChildren();
      const headline=document.createElement('p');
      headline.textContent=initial.connected?(error?'面板已接入，同步失败。':'面板已接入，同步完成。'):(error?'同步失败。':'同步完成。');
      status.append(headline);
      if(error){
        const reason=document.createElement('p');reason.className='wf-alert';reason.textContent=String(error);
        const retry=document.createElement('button');retry.type='button';retry.className='wf-button';retry.textContent='重新同步';retry.onclick=run;
        status.append(reason,retry);return;
      }
      const counts=[['新增线路',result.added],['更新',result.updated],['移除',result.removed],['未变更',result.unchanged],['跳过',result.skipped]];
      const totals=[['入站',result.inbounds],['客户端',result.clients],['出站',result.outbounds],['路由规则',result.rules]];
      status.insertAdjacentHTML('beforeend',`<dl class="wf-sync-counts">${counts.map(([label,count])=>`<div><dt>${label}</dt><dd>${esc(count??0)}</dd></div>`).join('')}</dl><p class="wf-hint">${totals.map(([label,count])=>`${label} ${esc(count??0)}`).join(' · ')}</p>`);
      for(const warning of list(result.warnings)){
        const text=document.createElement('p');text.className='wf-hint';text.textContent=warning;status.append(text);
      }
    }
    async function run() {
      if(el.dataset.busy==='1')return;
      el.dataset.busy='1';close.disabled=true;status.textContent='正在读取入站、客户端、出站和路由规则…';
      try{
        const result=await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/sync`,{method:'POST'});
        display(result);
        await ctx.refresh();
      }catch(err){display(null,err.message);}
      finally{delete el.dataset.busy;close.disabled=false;}
    }
    if(initial.result||initial.error)display(initial.result,initial.error);
    else run();
  }

  function removePanel(ctx,panel) {
    if(!panel)return ctx.toast('找不到该面板','error');
    const password=field('删除密码',input('password','输入删除密码','','type="password" required autocomplete="off"'));
    const note=`此操作会删除「${esc(panel.name)}」在本管理台的登记及其本地线路记录。远端 3x-ui 面板中的配置会保留。`;
    const confirm=modal(ctx,'删除面板','server',`<form id="wf-form" class="wf-form"><p>${note}</p>${password}<div class="wf-inline-error" role="alert"></div></form>`,submitFooter('确认删除'));
    confirm.querySelector('[data-wf-submit]').classList.add('wf-danger');
    bindForm(confirm,async form=>{
      await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}`,{method:'DELETE',body:{password:form.elements.password.value}});
      form.elements.password.value='';
      try{if(localStorage.getItem('nexus.line.panel')===panel.id)localStorage.removeItem('nexus.line.panel');}catch{}
      confirm.close();await ctx.refresh();ctx.toast('面板登记已删除');
    });
  }
  function operationResults(ctx,title,results) {
    const ok=results.filter(x=>x.ok).length;
    modal(ctx,title,'list-checks',`<p>${results.length} 项操作，成功 ${ok} 项，失败 ${results.length-ok} 项。</p><div>${results.map(r=>`<div class="wf-result"><div class="wf-spread"><strong>${esc(r.name||r.panelId)}</strong>${badge(r.ok?'成功':'失败',r.ok?'good':'fail')}</div><p>${esc(r.message||'')}</p></div>`).join('')}</div>`, '<button class="wf-button wf-primary" data-wf-close>完成</button>');
  }
  async function inboundResources(ctx,panel,withClients) {
    if(!withClients) return resources(ctx,panel,'inbounds');
    const [ins,clientResponse]=await Promise.all([resources(ctx,panel,'inbounds'),ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/clients`)]);
    const clients=list(clientResponse.clients);
    return ins.map(inbound=>({...inbound,settings:{...readSettings(inbound),clients:clients.filter(c=>list(c.inboundIds).some(id=>String(id)===String(inbound.id)))}}));
  }
  const operationNames={
    'create-inbound':'inbound.create','add-clients':'client.create','set-expiry':'client.expiry',
    'upsert-outbound':'outbound.upsert','upsert-route':'route.upsert'
  };
  const fieldDefs={
    outbound:{tag:'direct-custom',protocol:'freedom',settings:{}},
    rule:{type:'field',user:['用户名'],outboundTag:'用户名'}
  };
  function localDateTime(value) {
    if (!(Number(value)>0)) return '';
    const d=new Date(Number(value));
    return new Date(d.getTime()-d.getTimezoneOffset()*60000).toISOString().slice(0,19);
  }
  function expiryField(value,allowPaste) {
    const local=localDateTime(value);
    if (!allowPaste) return field('到期时间',input('expiry','',local.slice(0,16),'type="datetime-local"'),'使用当前设备时区，留空为永久有效。');
    const typed=input('expiry','2027-04-22 12:36:10',local.replace('T',' '),'autocomplete="off" spellcheck="false"');
    const picker=input('expiryPicker','',local,'type="datetime-local" step="1" aria-label="从日历选择到期时间" disabled');
    return `<div class="wf-expiry-editor">${field('到期时间',typed,'也可粘贴 2026/12/4 18:01:13。按当前设备时区计算，留空为永久有效。')}
      <details><summary>从日历选择</summary>${picker}</details></div>`;
  }
  function bindExpiryPicker(form) {
    const {expiry,expiryPicker}=form.elements;
    const details=expiryPicker.closest('details');
    details.ontoggle=()=>{expiryPicker.disabled=!details.open;};
    expiry.oninput=()=>{
      try { expiryPicker.value=localDateTime(window.Lines.parseLocalExpiry(expiry)); }
      catch { expiryPicker.value=''; }
    };
    expiryPicker.oninput=()=>{
      if (expiryPicker.validity.badInput) return;
      const value=expiryPicker.value.replace('T',' ');
      expiry.value=value.length===16?value+':00':value;
    };
  }
  function operationFields(action,params) {
    const expiry=expiryField(params.client?.expiryTime??params.inbound?.expiryTime,action==='client.expiry');
    if(action==='inbound.create')return `<div class="wf-grid">${field('入站名称',input('remark','例如 HK-Reality','','required maxlength="100"'))}${field('监听端口',input('ports','443, 8443','443','required'),'多个端口以逗号分隔。')}</div><div class="wf-grid">${field('Reality 服务名称（SNI）',input('serverName','www.example.com','','required spellcheck="false"'))}${field('目标地址（可选）',input('dest','SNI:443','','spellcheck="false"'))}</div>${expiry}<p class="wf-hint">VLESS · TCP · Reality，密钥由各目标面板独立生成。</p>`;
    if(action==='client.create')return `${field('客户端标识','<textarea name="emails" required placeholder="alice&#10;bob" spellcheck="false"></textarea>','每行一个，在每个选中面板上创建。')}${field('Flow','<select name="flow"><option value="xtls-rprx-vision">xtls-rprx-vision</option><option value="">无</option></select>')}${expiry}`;
    if(action.endsWith('.expiry'))return expiry+(action==='client.expiry'?'<p class="wf-hint">同一客户端的到期时间会应用到该面板上它关联的全部入站。</p>':'');
    const outbound=action==='outbound.upsert';
    const val=outbound?params.outbound:params.rule;
    const title=outbound?'出站配置 JSON':'路由规则 JSON';
    const help=outbound?'相同 tag 会更新已有出站，支持 JSON 对象数组。':params.index!==undefined?'更新当前面板选中的规则。':'新规则追加到路由列表末尾，支持 JSON 对象数组。';
    return field(title,`<textarea name="json" class="wf-code" rows="8" required spellcheck="false">${esc(JSON.stringify(val||fieldDefs[outbound?'outbound':'rule'],null,2))}</textarea>`,help);
  }
  function getOpInputs(form,action,params) {
    const val=n=>form.elements.namedItem(n)?.value.trim()||'';
    const expires=action==='client.expiry' ? window.Lines.parseLocalExpiry(form.elements.expiry)
      : (val('expiry') ? new Date(val('expiry')).getTime() : 0);
    if(!Number.isFinite(expires))throw new Error('到期时间格式无效');
    if(action==='inbound.create'){
      const ports=val('ports').split(/[\s,，]+/).filter(Boolean).map(s=>/^\d+$/.test(s)?Number(s):NaN);
      if(!ports.length||ports.some(p=>!Number.isInteger(p)||p<1||p>65535)||new Set(ports).size!==ports.length)throw new Error('端口必须是 1 到 65535 之间的整数，每个端口只能填写一次');
      if(!val('remark')||!val('serverName'))throw new Error('请填写入站名称和 Reality 服务名称');
      return ports.map(port=>({port,remark:val('remark')+(ports.length>1?`-${port}`:''),serverName:val('serverName'),...(val('dest')?{dest:val('dest')}:{ }),expiryTime:expires}));
    }
    if(action==='client.create'){
      const emails=val('emails').split('\n').map(i=>i.trim()).filter(Boolean);
      if(!emails.length||new Set(emails).size!==emails.length)throw new Error('请输入客户端标识并去除重复项');
      return emails.map(email=>({email,flow:val('flow'),expiryTime:expires}));
    }
    if(action.endsWith('.expiry'))return [{expiryTime:expires}];
    let data;
    try{data=JSON.parse(val('json'));}catch{throw new Error('JSON 格式有误，请检查引号、括号和逗号');}
    const items=Array.isArray(data)?data:[data];
    if(!items.length||items.some(i=>!i||Array.isArray(i)||typeof i!=='object'))throw new Error('请输入配置对象或非空对象数组');
    if(action==='outbound.upsert'){
      if(items.some(i=>typeof i.tag!=='string'||!i.tag.trim()||typeof i.protocol!=='string'||!i.protocol.trim()))throw new Error('每条出站都需要填写 tag 和 protocol');
      return items.map(outbound=>({outbound}));
    }
    if(params.index!==undefined&&items.length!==1)throw new Error('编辑现有规则时请填写一个规则对象');
    return items.map(rule=>({rule,...(params.index!==undefined?{index:params.index,expectedRule:params.rule}:{})}));
  }
  function targetRows(ctx,panelId,locked=false) {
    const panels=ctx.state.panels.filter(p=>!locked||p.id===panelId);
    return panels.map(p=>`<div class="wf-target" data-panel="${esc(p.id)}"><div class="wf-target-row"><label class="wf-check"><input type="checkbox" data-target value="${esc(p.id)}" ${(p.id===panelId||(!panelId&&panels.length===1))?'checked':''}>${esc(p.name)}</label><span class="wf-meta">${esc(p.region||'')}</span></div><div class="wf-target-extra"></div></div>`).join('')||'<div class="wf-empty">尚未接入面板</div>';
  }
  function operation(ctx,action,params={},chooser=false) {
    const needsInbound=action.startsWith('client.')||action==='inbound.expiry';
    const isClientExpiry=action==='client.expiry';
    const locked=action==='route.upsert'&&params.index!==undefined;
    const area=`<div class="wf-spread"><b>目标面板</b><label class="wf-check"><input type="checkbox" data-all>全选</label></div><div class="wf-targets">${targetRows(ctx,params.panelId,locked)}</div>`;
    const selector=chooser?field('操作类型',`<select name="action">${Object.entries(labels).filter(([k])=>k!=='backup').map(([k,v])=>`<option value="${k}" ${k===action?'selected':''}>${esc(v)}</option>`).join('')}</select>`):'';
    const el=modal(ctx,chooser?'批量操作':labels[action],'layers',`<form id="wf-form" class="wf-form">${selector}${operationFields(action,params)}${area}<div class="wf-inline-error" role="alert"></div></form>`,submitFooter('执行操作'));
    const form=el.querySelector('form');
    if (isClientExpiry) bindExpiryPicker(form);
    if(chooser)form.elements.action.onchange=e=>operation(ctx,e.target.value,{},true);
    const checks=[...form.querySelectorAll('[data-target]')],all=form.querySelector('[data-all]');
    const loads=new Map();
    const syncAll=()=>{const n=checks.filter(c=>c.checked).length;all.checked=n>0&&n===checks.length;all.indeterminate=n>0&&n<checks.length;};
    async function selectTarget(check) {
      syncAll();
      if(!needsInbound)return;
      const extra=check.closest('.wf-target').querySelector('.wf-target-extra');
      const generation=(Number(extra.dataset.generation)||0)+1;
      extra.dataset.generation=String(generation);
      if(!check.checked){extra.innerHTML='';return;}
      const p=ctx.state.panels.find(i=>i.id===check.value);
      if(!p)return;
      extra.innerHTML='<span class="wf-meta">正在读取目标入站…</span>';
      try {
        if(!loads.has(p.id))loads.set(p.id,inboundResources(ctx,p,isClientExpiry));
        let ins=await loads.get(p.id);
        if(generation!==Number(extra.dataset.generation))return;
        if(action.endsWith('.expiry')){
          const options=new Map();
          for(const inbound of ins){
            if(isClientExpiry){
              for(const client of list(readSettings(inbound).clients)){
                if(client.email && !options.has(client.email))options.set(client.email,{inboundId:inbound.id,email:client.email,label:client.email});
              }
            }else{
              options.set(inbound.id,{inboundId:inbound.id,label:`${inbound.remark||'入站'} · :${inbound.port} · #${inbound.id}`});
            }
          }
          if(!options.size){extra.innerHTML='<p class="wf-hint">该面板暂无可选对象。</p>';return;}
          extra.innerHTML='<label class="wf-check"><input type="checkbox" data-select-items>全选本面板对象</label><div class="wf-expiry-items">'+[...options.values()].map(item=>{
            const preselect=params.panelId===p.id&&(isClientExpiry?params.client?.email===item.email:params.inbound?.id===item.inboundId);
            return `<label class="wf-check"><input type="checkbox" data-item-id="${esc(item.inboundId)}" data-email="${esc(item.email||'')}" ${preselect?'checked':''}>${esc(item.label)}</label>`;
          }).join('')+'</div>';
          const toggle=extra.querySelector('[data-select-items]'),items=[...extra.querySelectorAll('[data-item-id]')];
          const sync=()=>{const count=items.filter(item=>item.checked).length;toggle.checked=count===items.length;toggle.indeterminate=count>0 && count<items.length;};
          toggle.onchange=()=>{items.forEach(item=>item.checked=toggle.checked);sync();};
          items.forEach(item=>item.onchange=sync);sync();return;
        }
        ins=ins.filter(x=>x.protocol==='vless');
        extra.innerHTML=`<select aria-label="${esc(p.name)} 目标入站" data-inbound required><option value="">选择目标入站</option>${ins.map(i=>`<option value="${esc(i.id)}" ${String(params.inbound?.id)===String(i.id)&&params.panelId===p.id?'selected':''}>${esc(i.remark||'入站')} · :${esc(i.port)} · #${esc(i.id)}</option>`).join('')}</select>`;
        const sel=extra.querySelector('[data-inbound]');
        if(ins.length===1)sel.value=String(ins[0].id);
        if(!ins.length)extra.insertAdjacentHTML('beforeend','<p class="wf-hint">该面板暂无可用入站。</p>');
      } catch(e){
        loads.delete(p.id);
        if(generation===Number(extra.dataset.generation))extra.innerHTML=`<span class="wf-alert">${esc(e.message)}，可重新勾选以重试。</span>`;
      }
    }
    checks.forEach(c=>{c.onchange=()=>selectTarget(c);if(c.checked)selectTarget(c);});
    all.onchange=()=>{const on=all.checked;checks.forEach(c=>{if(c.checked!==on){c.checked=on;selectTarget(c);}});};syncAll();
    bindForm(el,async()=>{
      const selected=checks.filter(x=>x.checked);
      if(!selected.length)throw new Error('请选择至少一个目标面板');
      const base=getOpInputs(form,action,params);
      const targets=selected.flatMap(c=>{
        const row=c.closest('.wf-target'),lookup={};
        if(action.endsWith('.expiry')){
          const chosen=[...row.querySelectorAll('[data-item-id]:checked')];
          if(!chosen.length)throw new Error('请在每个目标面板勾选需要修改的对象');
          return chosen.map(item=>({panelId:c.value,payload:{...base[0],inboundId:Number(item.dataset.itemId),...(isClientExpiry?{email:item.dataset.email}:{})}}));
        }
        if(needsInbound){
          const id=row.querySelector('[data-inbound]')?.value;
          if(!id)throw new Error('请为每个目标面板选择入站');
          lookup.inboundId=Number(id);
        }
        return base.map(payload=>({panelId:c.value,payload:{...payload,...lookup}}));
      });
      if(targets.length>512)throw new Error('每次最多执行 512 项操作，请分批提交');
      const results=list((await ctx.api('/api/batch',{method:'POST',body:{action,targets}})).results);
      el.close();operationResults(ctx,labels[action],results);await ctx.refresh();
    });
  }
  function saveBlob(blob,name) {
    const a=document.createElement('a'),url=URL.createObjectURL(blob);
    a.href=url;a.download=name;document.body.append(a);a.click();a.remove();
    setTimeout(()=>URL.revokeObjectURL(url),60000);
  }
  function downloadName(header) {
    const encoded=header?.match(/(?:^|;)\s*filename\*\s*=\s*UTF-8'[^']*'([^;]+)/i);
    let name='';
    if(encoded){try{name=decodeURIComponent(encoded[1].trim());}catch{}}
    if(!name){
      const plain=header?.match(/(?:^|;)\s*filename\s*=\s*(?:"([^"]+)"|([^;]+))/i);
      name=plain?.[1]||plain?.[2]?.trim()||'';
    }
    name=name.split(/[/\\]/).pop()?.trim();
    if(!name)throw new Error('服务器未返回备份文件名，请刷新后重试');
    return name;
  }
  function backup(ctx,panelId,immediate=false) {
    const body=`<form id="wf-form" class="wf-form"><div class="wf-spread"><b>选择备份面板</b><label class="wf-check"><input type="checkbox" data-all>全选</label></div><div class="wf-targets">${targetRows(ctx,panelId,!!panelId)}</div><p class="wf-hint">${panelId?'下载此面板的原始数据库文件。':'每个面板的数据库将合并为一个 ZIP 文件，其中包含备份结果清单。'}</p><div class="wf-inline-error" role="alert"></div></form>`;
    const el=modal(ctx,'下载面板备份','database',body,submitFooter('下载备份'));
    const all=el.querySelector('[data-all]'),checks=[...el.querySelectorAll('[data-target]')];
    const sync=()=>{const n=checks.filter(c=>c.checked).length;all.checked=!!n&&n===checks.length;all.indeterminate=n>0&&n<checks.length;};
    if(!panelId)checks.forEach(c=>c.checked=true);
    checks.forEach(c=>c.onchange=sync);
    all.onchange=()=>checks.forEach(c=>c.checked=all.checked);sync();
    bindForm(el,async()=>{
      const ids=checks.filter(c=>c.checked).map(c=>c.value);
      if(!ids.length||ids.length>128)throw new Error('请选择 1 至 128 个面板');
      const now=new Date();
      const stamp=[now.getFullYear(),String(now.getMonth()+1).padStart(2,'0'),String(now.getDate()).padStart(2,'0')].join('-');
      const res=await fetch('/api/backup',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({panelIds:ids,mode:panelId?'single':'archive',date:stamp})});
      if(!res.ok)throw new Error((await res.json().catch(()=>({}))).error||`备份失败（${res.status}）`);
      const ok=Number(res.headers.get('x-backup-success')),fail=Number(res.headers.get('x-backup-failed'));
      const filename=downloadName(res.headers.get('Content-Disposition'));
      saveBlob(await res.blob(),filename);
      el.close();ctx.toast(fail?`已下载 ${ok} 个面板的备份，${fail} 个失败。详情见压缩包内清单。`:'备份已下载');await ctx.refresh();
    });
    if(immediate)el.querySelector('form').requestSubmit();
  }

  function identifiers(value) {
    if(typeof value==='string')return value?[value]:[];
    return list(value).filter(item=>typeof item==='string' && item);
  }
  function ruleSelector(rule) {
    const represented=['type','ruleTag','user','inboundTag','outboundTag','balancerTag'];
    return Object.entries(rule).filter(([key,value])=>!represented.includes(key) && value!=null)
      .map(([key,value])=>`${key}: ${Array.isArray(value)?value.join(', '):typeof value==='object'?JSON.stringify(value):value}`).join(' · ')||'—';
  }
  function outboundRows(panel,config) {
    const byTag=new Map();
    for(const rule of list(config.routing?.rules)){
      if(!rule?.outboundTag)continue;
      let matches=byTag.get(rule.outboundTag);
      if(!matches){matches={users:new Set(),inbounds:new Set()};byTag.set(rule.outboundTag,matches);}
      identifiers(rule.user).forEach(user=>matches.users.add(user));
      identifiers(rule.inboundTag).forEach(tag=>matches.inbounds.add(tag));
    }
    const outbounds=list(config.outbounds), tagCounts=new Map();
    for (const o of outbounds) tagCounts.set(o.tag,(tagCounts.get(o.tag)||0)+1);
    return outbounds.map(outbound=>{
      const matches=byTag.get(outbound.tag);
      const canTest=outbound.protocol==='socks' && typeof outbound.tag==='string' &&
        outbound.tag.trim()!=='' && tagCounts.get(outbound.tag)===1;
      return {panel,outbound,canTest,users:[...(matches?.users||[])],inboundTags:[...(matches?.inbounds||[])]};
    });
  }
  function outboundTests(ctx,root,rowAt) {
    const controls=root.querySelector('[data-outbound-tests]');
    controls.innerHTML=`<div class="wf-test-controls">
      <div class="wf-test-modes" role="group" aria-label="延迟测试方式">
        <button type="button" data-test-mode="tcp" aria-pressed="true" title="连接 SOCKS5 服务器的 TCP 端口">TCP</button>
        <button type="button" data-test-mode="http" aria-pressed="false" title="通过 SOCKS5 出站请求面板的测试网址">HTTP</button>
      </div><button type="button" class="wf-button" data-test-all disabled>${ctx.icon('play',15)}全部测试</button>
      </div><small class="wf-test-progress" data-test-progress role="status"></small>`;
    const all=controls.querySelector('[data-test-all]'), progress=controls.querySelector('[data-test-progress]');
    const modeButtons=[...controls.querySelectorAll('[data-test-mode]')];
    const results={tcp:new Map(),http:new Map()};
    let mode='tcp', targets=[], busy=false;
    function update() {
      if (!root.isConnected) return;
      all.disabled=busy || !targets.length;
      all.title=`测试当前筛选中的 ${targets.length} 个 SOCKS5 出站（含所有分页）`;
      for (const button of modeButtons) {
        button.disabled=busy;
        button.setAttribute('aria-pressed',String(mode===button.dataset.testMode));
      }
      for (const cell of root.querySelectorAll('[data-test-result]')) {
        const row=rowAt(Number(cell.dataset.testResult)), result=results[mode].get(row);
        const button=cell.closest('tr').querySelector('[data-wf-row="outbound.test"]');
        button.disabled=busy || !row.canTest;
        let reason='';
        if (row.outbound.protocol!=='socks') reason='仅支持 SOCKS5 出站';
        else if (!row.canTest) reason='测试需要唯一的出站标签';
        button.title=reason || `测试 ${row.outbound.tag} 的 ${mode.toUpperCase()} 延迟`;
        button.setAttribute('aria-label',button.title);
        if (result?.error) {
          cell.innerHTML=`${badge('失败','fail')}<small class="wf-test-error">${esc(result.error)}</small>`;
        } else if (result?.latencyMs!=null) {
          cell.innerHTML=badge(`${result.latencyMs} ms`,'good');
        } else {
          cell.textContent=result?.status || '—';
        }
      }
    }
    async function probe(group,batch) {
      const url=`/api/panels/${encodeURIComponent(group[0].panel.id)}/outbounds/test`;
      if (!batch) {
        const tag=group[0].outbound.tag;
        const reply=await ctx.api(url,{method:'POST',body:{tag,mode}});
        return [{tag,latencyMs:reply?.latencyMs}];
      }
      const tags=group.map(row=>row.outbound.tag);
      const reply=await ctx.api(url+'-batch',{method:'POST',body:{tags,mode}});
      return list(reply?.results);
    }
    async function run(items,batch) {
      if (busy || !items.length) return;
      busy=true;
      const byPanel=new Map(), records=results[mode];
      let done=0, failed=0;
      for (const row of items) {
        if (!byPanel.has(row.panel.id)) byPanel.set(row.panel.id,[]);
        byPanel.get(row.panel.id).push(row);
        records.set(row,{status:'等待测试'});
      }
      const remaining=byPanel.values(), chunkSize=mode==='tcp' ? 50 : 3;
      progress.textContent=`已测 0 / ${items.length}`;
      async function worker() {
        for (const panelRows of remaining) {
          for (let start=0;start<panelRows.length;start+=chunkSize) {
            if (!root.isConnected) return;
            const group=panelRows.slice(start,start+chunkSize);
            for (const row of group) records.set(row,{status:'测试中…'});
            update();
            try {
              const replies=await probe(group,batch);
              const byTag=new Map(replies.map(result=>[result.tag,result]));
              for (const row of group) {
                const result=byTag.get(row.outbound.tag);
                if (!result?.error && Number.isSafeInteger(result?.latencyMs) && result.latencyMs>=0) {
                  records.set(row,{latencyMs:result.latencyMs});
                } else {
                  records.set(row,{error:result?.error || '面板未返回有效延迟'});
                  failed++;
                }
              }
            } catch (err) {
              for (const row of group) records.set(row,{error:err.message || '测试失败'});
              failed+=group.length;
            }
            done+=group.length;
            progress.textContent=`已测 ${done} / ${items.length}${failed ? ' · 失败 '+failed : ''}`;
            update();
          }
        }
      }
      try {
        await Promise.all(Array.from({length:Math.min(3,byPanel.size)},worker));
      } finally {
        busy=false;
        update();
      }
    }
    for (const button of modeButtons) button.onclick=()=>{
      if (busy) return;
      mode=button.dataset.testMode;
      progress.textContent='';
      update();
    };
    all.onclick=()=>run(targets,true);
    return {
      setScope(filtered) { targets=filtered.map(item=>item.r).filter(row=>row.canTest); update(); },
      single(row) { if (row.canTest) return run([row],false); }
    };
  }
  function repairRoutes(ctx,panelId) {
    const options=ctx.state.panels.map(p=>`<option value="${esc(p.id)}" ${p.id===panelId?'selected':''}>${esc(p.name)}</option>`).join('');
    const el=modal(ctx,'修正旧版线路路由','route',`<form id="wf-form" class="wf-form">${field('目标面板',`<select name="panelId" required><option value="">请选择面板</option>${options}</select>`)}<p>将可识别的旧版线路规则改为按客户端用户名匹配，指向该线路的出站。</p><div class="wf-inline-error" role="alert"></div></form>`,submitFooter('修正规则'));
    const select=el.querySelector('select');
    if(ctx.state.panels.length===1)select.value=ctx.state.panels[0].id;
    el.querySelector('[data-wf-submit]').disabled=!ctx.state.panels.length;
    bindForm(el,async()=>{
      const panel=ctx.state.panels.find(p=>p.id===select.value);
      if(!panel)throw new Error('请选择目标面板');
      const result=await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/repair-user-routes`,{method:'POST'});
      el.close();
      operationResults(ctx,'路由修正结果',[{name:panel.name,ok:true,message:`已修正 ${result.changed} 项，跳过 ${result.skipped} 项。`}]);
      await ctx.refresh();
    });
  }
  const headings={
    panels:['面板名称','备注','面板入口','地区','连接状态','入站','客户端','操作'],
    backups:['面板名称','地区','连接状态','操作'],
    inbounds:['入站名称','所属面板','协议','到期时间','状态','操作'],
    outbounds:['出站标签','规则中的用户','入站标签匹配','所属面板','协议','延迟','操作'],
    routes:['优先级','用户','出站','入站标签','其他条件','所属面板','操作'],
    activity:['操作类型','执行时间','涉及面板','执行结果','操作']
  };
  function panelAccess(ctx, panel) {
    let url;
    try { url = new URL(panel.url); } catch { return '—'; }
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) return '—';
    const base = url.pathname.replace(/\/+$/, '').replace(/\/panel(?:\/api)?$/, '');
    url.pathname = base + '/';
    return `<a class="wf-link wf-panel-visit" href="${esc(url.href)}" target="_blank" rel="noopener noreferrer" aria-label="访问 ${esc(panel.name)} 的面板（新标签页）">访问面板${ctx.icon('external-link',14)}</a>`;
  }
  async function fetchRows(ctx,page,onError) {
    const panels=ctx.state.panels;
    if(page==='panels'||page==='backups')return panels.map(panel=>({panel}));
    if(page==='activity'){
      const activity=(await ctx.api('/api/history')).history;
      return list(activity).map(event=>({event}));
    }
    const jobs=panels.map(panel=>async()=>{
      try{
        if(page==='inbounds')return (await resources(ctx,panel,'inbounds')).map(inbound=>({panel,inbound}));
        const cfg=await resources(ctx,panel,'config');
        if(page==='outbounds')return outboundRows(panel,cfg||{});
        return list(cfg?.routing?.rules).map((rule,index)=>({panel,rule,index}));
      }catch(e){onError(`${panel.name} · ${e.message}`);return [];}
    });
    const blocks=[];
    // Limit concurrent requests to upstream panels.
    for(let i=0;i<jobs.length;i+=6)blocks.push(...await Promise.all(jobs.slice(i,i+6).map(run=>run())));
    return blocks.flat();
  }
  function rowCells(ctx,page,row,index) {
    const {panel:p,inbound:i,client:c,outbound:o,rule:r,event:h}=row;
    const online=()=>p.online===true?badge('在线','good'):p.online===false?badge('离线','fail'):badge('待同步','neutral');
    if(page==='panels')return [entity(ctx,'server',p.name,p.url),
      `<button type="button" class="wf-note-editor" data-wf-row="panel-note" data-wf-index="${index}" title="${esc(p.note||'添加备注')}" aria-label="编辑 ${esc(p.name)} 的面板备注"><span>${esc(p.note||'添加备注')}</span>${ctx.icon('pencil',14)}</button>`,
      panelAccess(ctx,p),esc(p.region||'—'),online(),esc(p.inbounds??'—'),esc(p.clients??'—'),'<div class="wf-panel-actions">'+refLink('panel-detail',index,'详情')+refLink('panel-sync',index,'同步')+refLink('panel-remove',index,'删除')+'</div>'];
    if(page==='backups')return [entity(ctx,'database',p.name,p.url),esc(p.region||'—'),online(),refLink('download-one',index,'下载备份')];
    if(page==='inbounds')return [entity(ctx,'log-in',i.remark||'未命名入站',`:${i.port} · #${i.id}`),esc(p.name),badge(String(i.protocol||'—').toUpperCase()),esc(date(i.expiryTime||0)),badge(i.enable===false?'停用':'启用',i.enable===false?'neutral':'good'),refLink('inbound.expiry',index,'修改有效期')];
    if(page==='outbounds')return [entity(ctx,'log-out',o.tag||'未命名出站'),esc(row.users.join('、')||'—'),esc(row.inboundTags.join('、')||'—'),esc(p.name),badge(o.protocol||'—'),
      `<span class="wf-test-result" data-test-result="${index}">—</span>`,
      `<div class="wf-panel-actions"><button type="button" class="wf-icon-button wf-test-one" data-wf-row="outbound.test" data-wf-index="${index}" disabled>${ctx.icon('zap',15)}</button>${refLink('outbound.upsert',index,'编辑')}</div>`];
    if(page==='routes'){
      const match=ruleSelector(r);
      return [`<strong>#${row.index+1}</strong>`,esc(identifiers(r.user).join('、')||'—'),badge(r.outboundTag||(r.balancerTag?`均衡器 · ${r.balancerTag}`:'—')),esc(identifiers(r.inboundTag).join('、')||'—'),`<span class="wf-rule-condition" title="${esc(match)}">${esc(match)}</span>`,esc(p.name),refLink('route.upsert',index,'编辑')];
    }
    const result=list(h.results),n=result.filter(j=>j.ok).length;
    return [entity(ctx,'history',labels[h.action]||h.action),esc(date(h.ts)),esc(new Set(result.map(j=>j.panelId)).size),n===result.length?badge(`${n} 成功`,'good'):badge(`${n} 成功 · ${result.length-n} 失败`,'fail'),refLink('item-history',index,'查看结果')];
  }
  function matchSearch(page,row,query) {
    if(!query)return true;
    const {panel:p,inbound:i,client:c,outbound:o,rule:r,event:h}=row;
    let fields;
    switch(page){
      case 'activity':fields=[labels[h.action]||h.action,...list(h.results).map(x=>x.name)];break;
      case 'routes':fields=[p.name,ruleSelector(r),r.outboundTag,r.balancerTag,r.ruleTag,...identifiers(r.user),...identifiers(r.inboundTag)];break;
      case 'panels':fields=[p.name,p.region,p.url,p.note];break;
      case 'inbounds':fields=[p.name,i.remark,i.port,i.protocol,i.id];break;
      case 'outbounds':fields=[p.name,o.tag,o.protocol,...row.users,...row.inboundTags];break;
      default:fields=[p.name,p.region,p.url];
    }
    return fields.some(v=>String(v??'').toLowerCase().includes(query));
  }
  function render(page,host,ctx){
    if(page==='clients'){
      window.Clients.render(host,ctx,{open,modal,bindForm,submitFooter,operationResults});
      return;
    }
    const info=pages[page];
    if(!info)return;
    const [title,subtitle,unit,icon,actionText,action]=info;
    const extraAction=page==='inbounds'?'inbound.expiry':page==='clients'?'client.expiry':null;
    const scoped=['inbounds','clients','outbounds','routes'].includes(page);
    const headingBtns=`${page==='routes'?button(ctx,'修正旧版线路路由','repair-routes','route'):''}${extraAction?button(ctx,'批量改期',extraAction,'calendar-days'):''}${action?button(ctx,page==='backups'?'一键备份全部':actionText,page==='backups'?'download-all':action,page==='backups'?'download':'plus',true):''}`;
    host.innerHTML=`<div class="wf-page"><div class="wf-heading"><div><h1>${esc(title)}</h1><p>${ctx.state.panels.length} 个面板</p></div><div class="wf-actions">${headingBtns}</div></div><div class="wf-banners" role="status"></div><section class="wf-surface"><div class="wf-subhead"><div class="wf-row"><h2>${esc(subtitle)}</h2><span class="wf-count" data-count></span></div>${page==='backups'?button(ctx,'选择面板','backup','list-checks'):''}</div>${page==='backups'?`<div class="wf-backup-help">${ctx.icon('database')}<div><b>3x-ui 数据库备份</b><p>单个面板下载数据库，多选或全部备份下载 ZIP 压缩包。</p></div></div>`:''}<div class="wf-toolbar"><label class="wf-search">${ctx.icon('search',15)}<input data-search type="search" placeholder="搜索${esc(unit)}" aria-label="搜索${esc(unit)}"></label>${scoped?`<select class="wf-select" data-filter aria-label="按面板筛选"><option value="">全部面板</option>${ctx.state.panels.map(p=>`<option value="${esc(p.id)}">${esc(p.name)}</option>`).join('')}</select>`:''}</div><div data-list class="wf-loading">正在读取…</div><div data-pages></div></section></div>`;
    const root=host.querySelector('.wf-page'),listRoot=root.querySelector('[data-list]'),pag=root.querySelector('[data-pages]');
    let rows=[],slice=0,ready=false;
    let tests;
    if (page==='outbounds') {
      root.classList.add('wf-outbounds');
      root.querySelector('.wf-subhead').insertAdjacentHTML('beforeend','<div data-outbound-tests></div>');
      tests=outboundTests(ctx,root,index=>rows[index]);
    }
    root.querySelectorAll('[data-wf-open]').forEach(btn=>btn.onclick=()=>open(btn.dataset.wfOpen,ctx,{panelId:root.querySelector('[data-filter]')?.value||''}));
    const errors=[];
    function draw(){
      if(!ready)return;
      const limit=12,
        query=root.querySelector('[data-search]').value.trim().toLowerCase(),
        filter=root.querySelector('[data-filter]')?.value;
      const filtered=rows.map((r,j)=>({r,j})).filter(({r})=>(!filter||r.panel?.id===filter)&&matchSearch(page,r,query));
      const count=filtered.length, nPages=Math.max(Math.ceil(count/limit),1);
      slice=Math.min(slice,nPages-1);
      root.querySelector('[data-count]').textContent=String(count);
      listRoot.className='wf-table-wrap';
      listRoot.innerHTML=count?`<table class="wf-table"><thead><tr>${headings[page].map(h=>`<th scope="col">${esc(h)}</th>`).join('')}</tr></thead><tbody>${filtered.slice(slice*limit,(slice+1)*limit).map(({r,j})=>`<tr>${rowCells(ctx,page,r,j).map(v=>`<td>${v}</td>`).join('')}</tr>`).join('')}</tbody></table>`:empty(ctx,query||filter?'没有匹配的结果':page==='activity'?'暂无操作记录':ctx.state.panels.length?`暂无${unit}`:'尚未接入面板');
      pag.innerHTML=`<div class="wf-pagination"><span>共 ${count} 项</span>${nPages>1?`<div class="wf-row"><button class="wf-icon-button" data-prev aria-label="上一页" ${slice===0?'disabled':''}>${ctx.icon('chevron-left')}</button><span>${slice+1} / ${nPages}</span><button class="wf-icon-button" data-next aria-label="下一页" ${slice+1===nPages?'disabled':''}>${ctx.icon('chevron-right')}</button></div>`:''}</div>`;
      const prev=pag.querySelector('[data-prev]'),next=pag.querySelector('[data-next]');
      if(prev)prev.onclick=()=>{slice--;draw();};if(next)next.onclick=()=>{slice++;draw();};
      tests?.setScope(filtered);
    }
    root.querySelector('[data-search]').oninput=()=>{slice=0;draw();};
    const filter=root.querySelector('[data-filter]');
    if(filter)filter.onchange=()=>{slice=0;draw();};
    listRoot.addEventListener('click',ev=>{
      const btn=ev.target.closest('[data-wf-row]');
      if(!btn)return;
      const row=rows[Number(btn.dataset.wfIndex)],a=btn.dataset.wfRow;
      if(!row)return;
      if(a==='panel-note')return editPanelNote(ctx,row.panel,draw);
      if(a==='outbound.test')return tests.single(row);
      if(a==='item-history')return operationResults(ctx,labels[row.event.action]||row.event.action,list(row.event.results));
      open(a,ctx,{...row,panelId:row.panel.id});
    });
    fetchRows(ctx,page,msg=>errors.push(msg)).then(value=>{rows=value;}).catch(e=>errors.push(e.message)).finally(()=>{
      if(!root.isConnected)return;
      root.querySelector('.wf-banners').innerHTML=errors.map(msg=>`<p class="wf-alert">${esc(msg)}</p>`).join('');
      ready=true;draw();
    });
  }
  function open(action,ctx,payload={}) {
    let params=typeof payload==='string'?{panelId:payload}:payload||{};
    if(params.id&&!params.panelId)params={...params,panelId:params.id};
    switch(action){
      case 'add-panel':return addPanel(ctx);
      case 'panel-detail':return detail(ctx,ctx.state.panels.find(p=>p.id===params.panelId));
      case 'panel-sync':return showSync(ctx,ctx.state.panels.find(p=>p.id===params.panelId));
      case 'panel-remove':return removePanel(ctx,ctx.state.panels.find(p=>p.id===params.panelId));
      case 'backup':return backup(ctx,params.panelId);
      case 'download-all':
      case 'download-one':return backup(ctx,params.panelId,true);
      case 'repair-routes':return repairRoutes(ctx,params.panelId);
      case 'batch':return operation(ctx,'inbound.create',params,true);
      default:{
        const mapped=operationNames[action]||action;
        if(labels[mapped])return operation(ctx,mapped,params);
      }
    }
  }
  window.Workflows={render,open};
})();
