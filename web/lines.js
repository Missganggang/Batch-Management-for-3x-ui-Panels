(() => {
  'use strict';
  const durations=[1,2,3,6,12];
  const icon=window.NexusIcon;
  function monthOptions(name, required=false) {
    const type=required?'radio':'checkbox';
    return '<div class="ln-months">'+durations.map(m=>'<label><input type="'+type+'" name="'+name+'" value="'+m+'" '+(required&&m===1?'checked required':'')+'><span>'+(m===12?'1 年':m+' 个月')+'</span></label>').join('')+'</div>';
  }
  function quotaField({required = false} = {}) {
    const buttons = [0, 100, 200, 500].map(gb =>
      '<button type="button" data-quota="' + gb + '" aria-pressed="false">' + (gb ? gb + ' GB' : '不限') + '</button>'
    ).join('');
    return '<fieldset class="ln-limit-field"><legend>流量上限 <small>' +
      (required ? '每个所选客户端' : '每个客户端 · 留空不限') + '</small></legend>' +
      '<div class="ln-quota-input"><input type="text" inputmode="decimal" name="quotaGB" aria-label="流量上限（GB）" ' +
      'aria-required="' + required + '" placeholder="输入自定义额度" autocomplete="off"><span>GB</span></div>' +
      '<div class="ln-quota-options">' + buttons + '</div></fieldset>';
  }

  function bindQuota(form) {
    const input = form.querySelector('[name=quotaGB]');
    const buttons = [...form.querySelectorAll('[data-quota]')];
    const update = () => {
      const value = input.value.trim();
      buttons.forEach(button => button.setAttribute('aria-pressed', String(value !== '' && Number(value) === Number(button.dataset.quota))));
    };
    for (const button of buttons) {
      button.onclick = () => {
        input.value = button.dataset.quota;
        input.dispatchEvent(new Event('input', {bubbles:true}));
      };
    }
    input.addEventListener('input', update);
    update();
    return () => {
      const value = input.value.trim();
      if (!value && input.getAttribute('aria-required') === 'true') {
        input.focus();
        throw Error('请输入流量额度，或选择「不限」。');
      }
      if (!value) return 0;
      const bytes = Math.round(Number(value) * 1024 ** 3);
      if (!/^(?:\d+(?:\.\d*)?|\.\d+)$/.test(value) || !Number.isSafeInteger(bytes) ||
          (bytes === 0 && /[1-9]/.test(value))) {
        input.focus();
        throw Error('流量上限请填写有效的 GB 数值，可用小数，0 表示不限。');
      }
      return bytes;
    };
  }

  function createLimits() {
    return '<div class="ln-limits"><fieldset class="ln-limit-field"><legend>到期时间 <small>当前设备时区 · 留空永久</small></legend>' +
      '<input type="text" name="exactExpiry" aria-label="自定义到期时间" autocomplete="off" spellcheck="false" ' +
      'placeholder="例如 2027-04-22 12:36:10" title="支持 2027-04-22 12:36:10 或 2026/12/4 18:01:13">' +
      monthOptions('months') + '</fieldset>' + quotaField() + '</div>';
  }

  function parseLocalExpiry(input) {
    const value = input.value.trim();
    if (!value) return 0;
    const parts = /^(\d{4})([-/])(\d{1,2})\2(\d{1,2})\s+(\d{1,2}):(\d{2}):(\d{2})$/.exec(value);
    if (!parts) {
      input.focus();
      throw Error('到期时间请输入完整的日期和时分秒，例如 2027-04-22 12:36:10 或 2026/12/4 18:01:13。');
    }
    const [year, month, day, hour, minute, second] = [parts[1], ...parts.slice(3)].map(Number);
    const date = new Date(year, month - 1, day, hour, minute, second);
    if (date.getTime() <= 0 || date.getFullYear() !== year || date.getMonth() + 1 !== month ||
        date.getDate() !== day || date.getHours() !== hour || date.getMinutes() !== minute || date.getSeconds() !== second) {
      input.focus();
      throw Error('到期日期或时间无效，请检查年月日和时分秒。');
    }
    return date.getTime();
  }

  function bindLimits(form) {
    const exactExpiry = form.elements.exactExpiry;
    const options = [...form.querySelectorAll('input[name=months]')];
    for (const option of options) {
      option.addEventListener('change', () => {
        if (!option.checked) return;
        exactExpiry.value = '';
        options.forEach(other => { other.checked = other === option; });
      });
    }
    exactExpiry.addEventListener('input', () => options.forEach(option => { option.checked = false; }));
    const readQuota = bindQuota(form);
    return () => {
      const selected = options.find(option => option.checked);
      const term = selected ? {months:Number(selected.value)} : {expiryTime:parseLocalExpiry(exactExpiry)};
      return {...term, quotaBytes:readQuota()};
    };
  }
  function effectiveExpiry(line) {
    const limits=[line.expiryTime,line.inboundExpiryTime].filter(value=>Number(value)>0);
    return limits.length ? Math.min(...limits) : line.expiryTime;
  }
  const lineStatus=l=>l.status==='failed'?['bad','创建失败']:l.status==='pending'?['idle','处理中']:effectiveExpiry(l)>0&&effectiveExpiry(l)<=Date.now()?['bad','已到期']:l.enabled===false?['quiet','已停用']:l.online===true?['good','在线']:l.online===false?['quiet','离线']:['quiet','状态未知'];
  const date=t=>Number(t)>0?new Date(t).toLocaleString('zh-CN',{year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',second:'2-digit',hourCycle:'h23'}):t===0?'永久有效':'—';
  function traffic(n) {
    if(n==null||!Number.isFinite(Number(n)))return '—';
    const i=Math.min(5,Math.max(0,Math.floor(Math.log(Math.max(Number(n),1))/Math.log(1024))));
    return Number((Number(n)/1024**i).toFixed(i?1:0))+' '+['B','KB','MB','GB','TB','PB'][i];
  }
  const linesPerPage = 12;
  let page=null;
  function dialog(title,body,ctx) {
    document.querySelector('dialog.ln-dialog[open]')?.close();
    const d=document.createElement('dialog');
    d.className='ln-dialog';
    d.setAttribute('aria-labelledby','ln-dialog-title');
    d.innerHTML='<header><h2 id="ln-dialog-title">'+ctx.esc(title)+'</h2><button type="button" data-close aria-label="关闭">'+icon('x',18)+'</button></header>'+body;
    d.querySelector('[data-close]').onclick=()=>d.close();
    d.addEventListener('close',()=>d.remove(),{once:true});
    document.body.append(d);d.showModal();
    return d;
  }
  async function share(line,ctx) {
    const d=dialog('分享线路','<div class="ln-share"><div class="ln-share-name"></div><div class="ln-qr"><span>正在生成二维码…</span></div><label class="ln-share-link"><span>VLESS 链接</span><input readonly></label><div class="ln-share-buttons"><button class="nx-button" data-copy>'+icon('copy',15)+'复制链接</button><button class="nx-button nx-primary" data-img disabled>'+icon('clipboard',15)+'复制带名称的二维码</button></div><button class="ln-download" data-download disabled>'+icon('download',14)+'下载 PNG</button><p class="ln-message" role="status"></p></div>',ctx);
    d.classList.add('ln-share-dialog');
    const name=d.querySelector('.ln-share-name'),input=d.querySelector('input'),qr=d.querySelector('.ln-qr'),message=d.querySelector('[role=status]'),imgBtn=d.querySelector('[data-img]'),dl=d.querySelector('[data-download]');
    name.textContent=line.name;input.value=line.link;
    d.querySelector('[data-copy]').onclick=async()=>{
      try{await navigator.clipboard.writeText(line.link);message.textContent='链接已复制';}
      catch{input.focus();input.select();try{message.textContent=document.execCommand('copy')?'链接已复制':'链接已选中，请手动复制';}catch{message.textContent='链接已选中，请手动复制';}}
    };
    const controller=new AbortController();
    d.addEventListener('close',()=>controller.abort(),{once:true});
    try{
      const res=await fetch('/api/lines/qr',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({link:line.link}),signal:controller.signal});
      if(!res.ok)throw new Error((await res.json().catch(()=>({}))).error||'二维码生成失败');
      const blob=await res.blob(),src=URL.createObjectURL(blob);
      const image=new Image();
      try {image.src=src;await image.decode();}
      finally {URL.revokeObjectURL(src);}
      if(!d.open)return;
      const canvas=document.createElement('canvas');
      const side=image.naturalWidth;
      canvas.width=side;canvas.height=image.naturalHeight;
      const paint=canvas.getContext('2d');
      paint.fillStyle='#fff';paint.fillRect(0,0,side,canvas.height);
      paint.drawImage(image,0,0);
      canvas.setAttribute('aria-label',line.name+'的 VLESS 二维码');canvas.setAttribute('role','img');
      qr.replaceChildren(canvas);
      const card=document.createElement('canvas');
      card.width=side+48;
      const drawing=card.getContext('2d');
      const font='600 27px "Microsoft YaHei","Noto Sans CJK SC",sans-serif';
      drawing.font=font;
      const rows=[''];
      for(const char of line.name) {
        const j=rows.length-1;
        if(rows[j]&&drawing.measureText(rows[j]+char).width>side-24)rows.push(char);
        else rows[j]+=char;
      }
      const headerHeight=28+rows.length*38;
      card.height=headerHeight+canvas.height+32;
      drawing.fillStyle='#22252d';drawing.fillRect(0,0,card.width,card.height);
      drawing.font=font;drawing.textBaseline='top';drawing.fillStyle='#b5e487';
      rows.forEach((row,i)=>drawing.fillText(row,24,18+i*38));
      drawing.drawImage(canvas,24,headerHeight);
      const png=await new Promise((resolve,reject)=>card.toBlob(v=>v?resolve(v):reject(new Error('PNG 生成失败')),'image/png'));
      if(!d.open)return;
      imgBtn.disabled=false;dl.disabled=false;
      dl.onclick=()=>{
        const url=URL.createObjectURL(png),a=document.createElement('a');
        a.href=url;a.download=(line.name.replace(/[<>:"/\\|?*\x00-\x1f]/g,'_').slice(0,100)||'线路')+'-二维码.png';
        d.append(a);a.click();a.remove();setTimeout(()=>URL.revokeObjectURL(url),30000);
      };
      imgBtn.onclick=async()=>{
        try{
          if(!navigator.clipboard?.write||!window.ClipboardItem)throw new Error();
          await navigator.clipboard.write([new ClipboardItem({'image/png':png})]);message.textContent='带线路名称的二维码已复制';
        }catch{message.textContent='浏览器未允许复制图片，可使用「下载 PNG」。';dl.focus();}
      };
    }catch(error){if(d.open){qr.textContent='二维码暂不可用';message.textContent=error.message;}}
  }
  function createPage(host,ctx) {
    const root=document.createElement('div');root.className='ln-page';
    root.innerHTML='<div class="ln-heading"><div><h1>线路管理</h1><p><span class="ln-protocol">'+icon('shield-check',13)+'VLESS</span><span>TCP</span><i></i><span>Reality</span></p></div><button class="nx-button" data-refresh>'+icon('refresh-cw',14)+'刷新</button></div><section class="ln-compose"><div class="ln-compose-heading"><div class="ln-compose-icon">'+icon('route',21)+'</div><div><h2>新建线路</h2><p>同一个名称将用于入站、客户端和 SOCKS5 出站。</p></div></div><form class="ln-form"><div class="ln-fields"><label>线路名称<input name="name" required maxlength="80" autocomplete="off" placeholder="例如 中国台湾210.65.177.177"></label><label>SOCKS5 出口<span class="ln-secret"><input type="text" name="socks5" required autocomplete="off" spellcheck="false" placeholder="socks5://user:pass@host:port"><button type="button" data-visibility aria-label="隐藏 SOCKS5">'+icon('eye-off',16)+'</button></span></label></div>'+createLimits()+'<div class="ln-form-bottom"><label class="ln-panel-select"><span>目标面板</span><select name="panelId" aria-label="目标面板" disabled><option>正在读取面板…</option></select></label><div class="ln-create-actions"><button type="button" class="nx-button ln-batch-open" data-batch disabled>'+icon('layers',15)+'批量创建线路</button><button class="nx-button nx-primary ln-submit" type="submit" disabled>'+icon('plus',16)+'创建线路</button></div></div><div class="ln-panel-support"><button type="button" data-add>'+icon('server',14)+'接入面板</button><button type="button" data-host hidden>'+icon('settings-2',14)+'分享地址</button></div><div class="ln-form-error" role="alert"></div></form></section><div class="ln-alerts" aria-live="polite"></div><section class="ln-list"><div class="ln-list-head"><div><h2>我的线路</h2><b data-total>0</b></div><div class="ln-list-tools"><a class="ln-manage-traffic" href="#clients">管理流量</a><label class="ln-search">'+icon('search',15)+'<input type="search" placeholder="搜索线路" aria-label="搜索线路"></label></div></div><div class="ln-table-wrap"><table><thead><tr><th>线路名称</th><th>在线状态</th><th>已用流量 / 上限</th><th>到期时间</th><th>操作</th></tr></thead><tbody></tbody></table></div><div class="ln-empty" role="status">正在读取线路…</div>' +
      '<nav class="ln-pager" aria-label="我的线路分页" hidden><span data-line-range></span><div>' +
      '<button type="button" class="nx-button" data-line-prev aria-label="上一页">'+icon('chevron-left',14)+'</button>' +
      '<span data-line-page aria-live="polite"></span><button type="button" class="nx-button" data-line-next aria-label="下一页">'+icon('chevron-right',14)+'</button></div></nav></section>';
    host.replaceChildren(root);
    const model={root,ctx,panels:[],lines:[],listPage:1,errors:[],request:0,ready:false,creating:false,refreshing:false};
    const form=root.querySelector('form'),btn=form.querySelector('[type=submit]'),panelChoice=form.elements.panelId;
    const readLimits = bindLimits(form);
    root.querySelector('[data-batch]').onclick=()=>batchCreate(model);
    const addPanel=()=>window.Workflows.open('add-panel',ctx);
    root.querySelector('[data-add]').onclick=addPanel;
    root.querySelector('[data-host]').onclick=()=>editHost(model);
    root.querySelector('[data-refresh]').onclick=()=>load(model,true);
    root.querySelector('[type=search]').oninput=()=>{model.listPage=1;drawLines(model);};
    root.querySelector('[data-line-prev]').onclick=()=>{model.listPage--;drawLines(model);};
    root.querySelector('[data-line-next]').onclick=()=>{model.listPage++;drawLines(model);};
    form.querySelector('[data-visibility]').onclick=e=>{
      const show=form.elements.socks5.type==='password';
      form.elements.socks5.type=show?'text':'password';
      e.currentTarget.innerHTML=icon(show?'eye-off':'eye',16);e.currentTarget.setAttribute('aria-label',show?'隐藏 SOCKS5':'显示 SOCKS5');
    };
    panelChoice.onchange=()=>{
      form.querySelector('[data-host]').hidden=!panelChoice.value;
      form.querySelector('[role=alert]').textContent='';
      model.listPage=1;
      drawLines(model);
    };
    form.onsubmit=async e=>{
      e.preventDefault();if(model.creating)return;
      const output=form.querySelector('[role=alert]');
      if(!model.panels.some(p=>String(p.id)===panelChoice.value)){
        output.textContent=model.panels.length?'请选择一个目标面板，再创建线路。':'请先接入一个面板。';
        panelChoice.focus();return;
      }
      const body={panelId:panelChoice.value,name:form.elements.name.value.trim(),socks5:form.elements.socks5.value.trim()};
      if(!body.name||!body.socks5){output.textContent='请填写线路名称和 SOCKS5 出口';return;}
      if(/[\s/\\]/u.test(body.name)||['api','.','..'].includes(body.name)){
        output.textContent='请使用不含空格或斜杠的线路名称，避开保留名称 api、. 和 ..';form.elements.name.focus();return;
      }
      try { Object.assign(body, readLimits()); }
      catch(error) { output.textContent=error.message;return; }
      output.textContent='';model.creating=true;btn.disabled=true;btn.textContent='正在创建…';form.inert=true;
      try{
        const {line}=await ctx.api('/api/lines',{method:'POST',body});
        model.listPage=1;form.elements.socks5.value='';form.elements.name.value='';
        form.querySelectorAll('input[name=months]').forEach(choice=>choice.checked=false);
        form.elements.exactExpiry.value='';form.elements.quotaGB.value='';
        form.elements.quotaGB.dispatchEvent(new Event('input'));
        if(line?.link&&root.isConnected)share(line,ctx);
        else ctx.toast(line?.status==='failed'?'线路创建失败，请查看列表中的原因':'线路已提交');
      }catch(error){output.textContent=error.message;}
      finally{model.creating=false;form.inert=false;btn.innerHTML=icon('plus',16)+'创建线路';btn.disabled=!model.panels.length;await load(model);}
    };
    root.querySelector('tbody').onclick=e=>{
      const b=e.target.closest('[data-line]');if(!b)return;
      const line=model.lines.find(l=>String(l.id)===b.dataset.line);if(!line)return;
      if(b.dataset.op==='share')share(line,ctx);
      if(b.dataset.op==='renew')renew(line,model);
      if(b.dataset.op==='reality')repairReality(line,model);
      if(b.dataset.op==='retry'){
        form.elements.name.value=line.name;panelChoice.value=line.panelId;
        panelChoice.dispatchEvent(new Event('change'));
        form.elements.socks5.value='';form.querySelector('[role=alert]').textContent='重新填写这条线路原来的 SOCKS5 出口后提交。';
        form.elements.socks5.focus();form.scrollIntoView({block:'center',behavior:'smooth'});
      }
    };
    return model;
  }
  function updatePanels(model) {
    const form=model.root.querySelector('form'),select=form.elements.panelId;
    const previous=select.disabled?'':select.value;
    select.replaceChildren(new Option(model.panels.length?'全部面板':'请先接入面板',''));
    model.panels.forEach(p=>select.add(new Option(p.name,p.id)));
    if(model.panels.some(p=>String(p.id)===previous))select.value=previous;
    if(previous!==select.value)model.listPage=1;
    select.disabled=!model.panels.length;
    form.querySelector('[data-host]').hidden=!select.value;
    form.querySelector('.ln-panel-support').classList.toggle('ln-needs-panel',!model.panels.length);
    form.querySelector('[type=submit]').disabled=model.creating||!model.panels.length;
    form.querySelector('[data-batch]').disabled=model.creating||!model.panels.length;
  }
  function drawLines(model) {
    const {root,ctx,lines}=model,esc=ctx.esc;
    const select=root.querySelector('[name=panelId]');
    const panelId=select.disabled?'':select.value;
    const term=root.querySelector('[type=search]').value.trim().toLowerCase();
    const panelLines=panelId?lines.filter(l=>String(l.panelId)===panelId):lines;
    const list=panelLines.filter(l=>(l.name+' '+l.panelName).toLowerCase().includes(term));
    const pages = Math.max(1, Math.ceil(list.length / linesPerPage));
    model.listPage = Math.min(Math.max(1, model.listPage), pages);
    const start = (model.listPage - 1) * linesPerPage;
    const visible = list.slice(start, start + linesPerPage);
    const pager = root.querySelector('.ln-pager');
    pager.hidden = !list.length;
    pager.querySelector('[data-line-range]').textContent = list.length ? `${start + 1}–${start + visible.length} / ${list.length} 条线路` : '';
    pager.querySelector('[data-line-page]').textContent = `第 ${model.listPage} / ${pages} 页`;
    pager.querySelector('[data-line-prev]').disabled = model.listPage === 1;
    pager.querySelector('[data-line-next]').disabled = model.listPage === pages;
    const empty=root.querySelector('.ln-empty'),table=root.querySelector('.ln-table-wrap');
    root.querySelector('[data-total]').textContent=String(panelLines.length);
    empty.hidden=!!list.length;table.hidden=!list.length;
    empty.innerHTML=icon('route',24)+'<strong>'+(model.ready?(term?'没有匹配的线路':'暂无线路'):'无法读取线路')+'</strong><span>'+(model.ready?(term?'试试其他名称或面板。':'创建第一条线路后，可在这里查看流量和分享二维码。'):'点击刷新以重新连接。')+'</span>';
    const act=(l,op,title)=>'<button data-op="'+op+'" data-line="'+esc(l.id)+'">'+title+'</button>';
    root.querySelector('tbody').innerHTML=visible.map(l=>{
      const [tone,label]=lineStatus(l);
      const total=l.up!=null && l.down!=null ? traffic(Number(l.up)+Number(l.down)) : '—';
      const quota=l.quotaBytes==null?'—':l.quotaBytes===0?'不限':traffic(l.quotaBytes);
      const error=l.error?'<small class="ln-row-error">'+esc(l.error)+'</small>':'';
      return '<tr><td><div class="ln-line-name"><span>'+icon('route',17)+'</span><div><strong>'+esc(l.name)+'</strong><small>'+esc(l.panelName||'—')+'</small>'+error+'</div></div></td><td><span class="ln-status '+tone+'"><i></i>'+label+'</span></td><td><strong class="ln-used">'+total+' <span class="ln-quota-total">/ '+quota+'</span></strong><div class="ln-traffic"><span title="上行流量">'+icon('arrow-up',12)+traffic(l.up)+'</span><span title="下行流量">'+icon('arrow-down',12)+traffic(l.down)+'</span></div></td><td>'+esc(date(effectiveExpiry(l)))+'</td><td><div class="ln-row-actions">'+(l.status==='ready'?(l.link?act(l,'share',icon('external-link',13)+'分享'):'')+act(l,'renew','续期')+(l.name===l.clientName&&l.name===l.inboundName?act(l,'reality','检测与修复'):''):l.status==='failed'?act(l,'retry','重试'):'—')+'</div></td></tr>';
    }).join('');
  }
  async function load(model,manual=false) {
    const id=++model.request,{ctx,root}=model,refresh=root.querySelector('[data-refresh]');
    refresh.disabled=true;model.refreshing=true;
    const [panels,rows]=await Promise.allSettled([ctx.api('/api/panels'),ctx.api('/api/lines')]);
    if(model.request!==id||!root.isConnected)return;
    const errors=[];
    if(panels.status==='fulfilled'){model.panels=panels.value.panels || [];ctx.state.panels=model.panels;updatePanels(model);}
    else errors.push(panels.reason.message);
    if(rows.status==='fulfilled'){
      model.lines=rows.value.lines || [];model.ready=true;
      for(const e of rows.value.errors||[]){const p=model.panels.find(p=>p.id===e.panelId);errors.push((p?.name||'面板')+' · '+e.message);}
      ctx.state.updatedAt=Date.now();
    }else{
      errors.push(rows.reason.message);
      model.lines=model.lines.map(line=>({...line,online:null,up:null,down:null}));
    }
    if(panels.status==='fulfilled'){
      const panelIds=new Set(model.panels.map(p=>p.id));
      model.lines=model.lines.filter(line=>panelIds.has(line.panelId));
    }
    ctx.state.error=errors.join(' · ');
    root.querySelector('.ln-alerts').innerHTML=[...new Set(errors)].map(e=>'<p>'+icon('circle-alert',15)+ctx.esc(e)+'</p>').join('');
    refresh.disabled=false;model.refreshing=false;drawLines(model);
    if(manual&&!errors.length)ctx.toast('线路状态已刷新');
    const sync=document.getElementById('sync-label');
    if(sync){sync.classList.toggle('nx-sync-error',!!errors.length);sync.innerHTML='<i></i>'+(errors.length?'部分数据待同步':'最近同步 '+new Date().toLocaleTimeString('zh-CN',{hour12:false}));}
  }
  function validSocks5(value) {
    if(/[\u0000-\u001f\u007f]/u.test(value) || new TextEncoder().encode(value).length>4096)return false;
    let host,port,user='',pass='';
    try {
      if(value.includes('://')){
        const url=new URL(value);
        if(!['socks5:','socks5h:'].includes(url.protocol) || !['','/'].includes(url.pathname) || url.href.includes('?') || url.href.includes('#'))return false;
        host=url.hostname;port=url.port;user=decodeURIComponent(url.username);pass=decodeURIComponent(url.password);
      }else{
        const parts=/^(\[[^\]]+\]|[^:]+):([0-9]+)(?::([^:]*)(?::(.*))?)?$/u.exec(value);
        if(!parts)return false;
        [,host,port,user='',pass='']=parts;
      }
      if(!host || /[\s/@?#%\\]/u.test(host) || !new URL('http://'+host).hostname)return false;
      const bytes=new TextEncoder();
      return Number(port)>0 && Number(port)<=65535 && !!user===!!pass && bytes.encode(user).length<=255 && bytes.encode(pass).length<=255;
    }catch{return false;}
  }
  function readColumns(namesText,socksText,outcomes) {
    const split=text=>text.split(/\r\n?|\n/).map(row=>row.trim());
    const names=split(namesText),socks=split(socksText),used=new Set();
    const batch={lines:[],names:names.filter(Boolean).length,socks:socks.filter(Boolean).length,error:''};
    for(let i=0;i<Math.max(names.length,socks.length);i++){
      const name=names[i],socks5=socks[i];
      if(!name&&!socks5)continue;
      let error='';
      if(!name || !socks5)error=name?'缺少 SOCKS5':'缺少线路名称';
      else if([...name].length>80 || /[\s/\\\u0000-\u001f\u007f]/u.test(name) || ['api','.','..'].includes(name))error='线路名称无效';
      else if(used.has(name))error='线路名称重复';
      else if(outcomes.get(name)?.ok)error='该线路已创建';
      else if(!validSocks5(socks5))error='SOCKS5 格式无效，请检查地址、端口和账号';
      if(error){batch.error='第 '+(i+1)+' 行'+error;break;}
      used.add(name);batch.lines.push({name,socks5});
    }
    return batch;
  }
  function batchCreate(model) {
    const {ctx,root}=model,active=root.querySelector('[name=panelId]').value;
    const d=dialog('批量创建线路',
      '<form class="ln-small-form ln-batch-form">'+
        '<div class="ln-paste-columns">'+
          '<label><span>线路名称 <output data-name-count>0 条</output></span><textarea name="names" rows="7" wrap="off" spellcheck="false" autocomplete="off" placeholder="香港-01&#10;东京-02"></textarea></label>'+
          '<label><span>SOCKS5 出口 <output data-socks-count>0 条</output></span><textarea name="socks" rows="7" wrap="off" spellcheck="false" autocomplete="off" placeholder="socks5://user:pass@hk.example.com:1080&#10;jp.example.com:1080:user:pass"></textarea></label>'+
        '</div><p data-paste-help>按行配对。第 1 个名称使用右侧第 1 行的出口。</p><div class="ln-form-error" data-pair-error aria-live="polite"></div>'+
        '<label class="ln-panel-select">目标面板<select name="panelId" required></select></label>'+
        createLimits()+
        '<div data-results aria-live="polite"></div><div class="ln-form-error" role="alert"></div>'+
        '<div class="ln-batch-footer"><button class="nx-button" type="button" data-clients hidden>查看客户端</button><button class="nx-button nx-primary" type="submit">创建线路</button></div>'+
      '</form>',ctx);
    d.classList.add('ln-batch-dialog');
    const form=d.querySelector('form'),names=form.elements.names,socks=form.elements.socks,panel=form.elements.panelId;
    const submit=form.querySelector('[type=submit]'),message=form.querySelector('[role=alert]'),results=form.querySelector('[data-results]');
    const clients=form.querySelector('[data-clients]');
    const outcomes=new Map();
    let submitted=false,processing=false;
    panel.add(new Option('请选择目标面板',''));
    model.panels.forEach(p=>panel.add(new Option(p.name,p.id)));
    panel.value=active;
    const readLimits = bindLimits(form);
    function validate(){
      const batch=readColumns(names.value,socks.value,outcomes);
      form.querySelector('[data-name-count]').textContent=batch.names+' 条';
      form.querySelector('[data-socks-count]').textContent=batch.socks+' 条';
      form.querySelector('[data-pair-error]').textContent=batch.error;
      submit.disabled=processing||!!batch.error||!batch.lines.length||!panel.value;
      return batch;
    }
    for(const [editor,other] of [[names,socks],[socks,names]]){
      editor.addEventListener('input',()=>{message.textContent='';validate();});
      editor.addEventListener('scroll',()=>{if(other.scrollTop!==editor.scrollTop)other.scrollTop=editor.scrollTop;});
    }
    panel.addEventListener('change',validate);
    validate();
    d.addEventListener('cancel',event=>{if(processing)event.preventDefault();});
    clients.onclick=()=>{d.close();ctx.navigate('clients');};
    form.onsubmit=async event=>{
      event.preventDefault();
      if(processing)return;
      const {lines,error}=validate();
      if(error||!lines.length||!panel.value)return;
      let limits;
      try { limits=readLimits(); }
      catch(err) { message.textContent=err.message;return; }
      processing=true;form.inert=true;d.querySelector('[data-close]').disabled=true;
      submit.textContent='正在创建…';message.textContent='';
      try{
        const body={panelId:panel.value,...limits,lines};
        const response=await ctx.api('/api/lines/batch',{method:'POST',body});
        if(!Array.isArray(response.results))throw Error('未收到逐行结果，请刷新线路列表核对');
        const byName=new Map(response.results.map(item=>[item.name,item]));
        const failed=[];
        for(const entry of lines){
          const result=byName.get(entry.name)||{name:entry.name,ok:false,message:'面板未返回该线路的结果'};
          outcomes.set(entry.name,result);
          if(!result.ok)failed.push(entry);
        }
        const rows=[...outcomes.values()];
        const success=rows.filter(item=>item.ok).length;
        const summary=document.createElement('strong');summary.textContent='成功 '+success+' 条 · 失败 '+(rows.length-success)+' 条';
        const list=document.createElement('ul');list.className='ln-batch-results';
        for(const row of rows){
          const li=document.createElement('li'),label=document.createElement('b'),detail=document.createElement('span');
          li.className=row.ok?'is-ok':'is-error';label.textContent=row.name;
          detail.textContent=row.ok?'已创建':(row.message||'创建失败');li.append(label,detail);list.append(li);
        }
        results.replaceChildren(summary,list);
        names.value=failed.map(item=>item.name).join('\n');
        socks.value=failed.map(item=>item.socks5).join('\n');
        panel.disabled=true;
        if(!failed.length)form.querySelectorAll('.ln-limit-field').forEach(field=>field.disabled=true);
        clients.hidden=!success;submitted=true;
        const mainPanel=root.querySelector('[name=panelId]');
        mainPanel.value=panel.value;
        mainPanel.dispatchEvent(new Event('change'));
        if(failed.length){
          form.querySelector('[data-paste-help]').textContent='已保留失败的线路和出口。修改后可再次提交。';
        }else{
          form.querySelector('.ln-paste-columns').hidden=true;
          form.querySelector('[data-paste-help]').hidden=true;
          submit.hidden=true;
        }
        await load(model);
      }catch(error){message.textContent=error.message;}
      finally{
        processing=false;form.inert=false;d.querySelector('[data-close]').disabled=false;
        submit.textContent=submitted?'重试失败线路':'创建线路';
        validate();
      }
    };
  }
  function repairReality(line,model) {
    const {ctx}=model;
    const d=dialog('线路检测与修复',
      '<form class="ln-small-form"><h3>'+ctx.esc(line.name)+'</h3>'+
      '<label>Reality 目标<input name="target" type="text" spellcheck="false" autocomplete="off" placeholder="使用服务器默认目标"></label>'+
      '<p>填写域名和端口，或留空使用部署时设置的目标。面板验证通过后会更新入站的 SNI 列表和分享链接。</p>'+
      '<div class="ln-repair-actions"><button class="nx-button" type="button" data-test>测试 SOCKS5 出口</button>'+
      '<button class="nx-button nx-primary" type="submit">检测并修复 Reality</button></div>'+
      '<p role="status" aria-live="polite"></p><button class="nx-button" data-share-fixed type="button" hidden>查看新链接和二维码</button></form>',ctx);
    const form=d.querySelector('form'),test=form.querySelector('[data-test]'),status=form.querySelector('[role=status]'),
      close=d.querySelector('[data-close]'),show=form.querySelector('[data-share-fixed]');
    const base='/api/lines/'+encodeURIComponent(line.id);
    let repaired, busy=false;
    show.onclick=()=>{d.close();share(repaired,ctx);};
    d.addEventListener('cancel',e=>{if(busy)e.preventDefault();});
    async function run(testOnly){
      if(busy)return;
      busy=true;form.inert=true;close.disabled=true;
      status.textContent=testOnly?'面板正在经 SOCKS5 出口访问测试网址…':'面板正在检测目标并更新 SNI…';
      try{
        if(testOnly){
          const response=await ctx.api(base+'/test-outbound',{method:'POST'});
          status.textContent='SOCKS5 出口可用，面板到测试网址的延迟为 '+response.delay+' ms。';
        }else{
          const response=await ctx.api(base+'/repair-reality',{method:'POST',body:{target:form.elements.target.value.trim()}});
          repaired=response.line;show.hidden=false;
          status.textContent='目标 '+response.target+' 已通过检测，当前有 '+response.sniCount+' 个可用 SNI。请在客户端重新导入分享链接或二维码。';
          await load(model);
        }
      }catch(err){status.textContent=err.message;}
      finally{busy=false;form.inert=false;close.disabled=false;}
    }
    test.onclick=()=>run(true);
    form.onsubmit=e=>{e.preventDefault();run(false);};
  }
  function editHost(model) {
    const {ctx,root}=model,p=model.panels.find(p=>String(p.id)===root.querySelector('[name=panelId]').value);
    if(!p)return;
    const d=dialog('修改分享地址','<form class="ln-small-form"><label>公网域名或 IP<input name="publicHost" autocomplete="off" placeholder="例如 edge.example.com"></label><p>面板 '+ctx.esc(p.name)+' 的 VLESS 链接将使用此地址。留空则使用面板域名。</p><div class="ln-form-error" role="alert"></div><button class="nx-button nx-primary">保存地址</button></form>',ctx);
    const f=d.querySelector('form');f.elements.publicHost.value=p.publicHost||'';
    f.onsubmit=async e=>{
      e.preventDefault();if(f.inert)return;f.inert=true;
      try{await ctx.api('/api/panels/'+encodeURIComponent(p.id)+'/public-host',{method:'POST',body:{publicHost:f.elements.publicHost.value.trim()}});d.close();await load(model);ctx.toast('分享地址已更新');}
      catch(err){f.querySelector('[role=alert]').textContent=err.message;}finally{f.inert=false;}
    };
  }
  function renew(line,model) {
    const {ctx}=model;
    const d=dialog('线路续期','<form class="ln-small-form"><h3>'+ctx.esc(line.name)+'</h3><p>当前到期日 '+date(line.expiryTime)+'。已到期的线路从今天起算。</p>'+(line.inboundExpiryTime>0?'<p class="wf-hint">入站本身将于 '+date(line.inboundExpiryTime)+' 到期。此处续期仅更新客户端期限。</p>':'')+(line.sharedClient?'<p class="wf-note">该客户端关联多个入站，续期将同时作用于这些入站。</p>':'')+'<fieldset><legend>续期时长</legend>'+monthOptions('months',true)+'</fieldset><div class="ln-form-error" role="alert"></div><button class="nx-button nx-primary">确认续期</button></form>',ctx);
    const f=d.querySelector('form');
    f.onsubmit=async e=>{
      e.preventDefault();if(f.inert)return;f.inert=true;
      try{await ctx.api('/api/lines/'+encodeURIComponent(line.id)+'/expiry',{method:'POST',body:{months:Number(f.elements.months.value)}});d.close();await load(model);ctx.toast('到期日期已更新');}
      catch(err){f.querySelector('[role=alert]').textContent=err.message;}finally{f.inert=false;}
    };
  }
  async function mount(host,ctx) {
    if(!page?.root.isConnected)page=createPage(host,ctx);
    await load(page);
  }
  window.Lines={mount,share,quotaField,bindQuota,parseLocalExpiry};
  setInterval(()=>{if(page?.root.isConnected&&!document.hidden&&!page.refreshing&&!page.creating&&!document.querySelector('dialog[open]'))load(page);},30000);
})();
