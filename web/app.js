(() => {
  'use strict';

  const icon = window.NexusIcon;
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const groups = [
    ['工作台', [['lines','线路管理','route'],['clients','客户端管理','users-round'],['panels','面板管理','server'],['backups','数据备份','database-backup']]],
    ['高级管理', [['overview','面板概览','layout-dashboard'],['inbounds','入站管理','log-in'],['outbounds','出站管理','log-out'],['routes','路由管理','workflow'],['activity','操作记录','history']]]
  ];
  const pages = new Map(groups.flatMap(([,rows]) => rows.map(([id,label]) => [id,label])));
  const gb = 1024 ** 3;
  const hour = 3600000;
  const demoPanels = [
    ['hk-01','香港核心节点','中国香港','HK',8,58,784],
    ['sg-01','新加坡边缘节点','新加坡','SG',6,46,639],
    ['jp-01','东京核心节点','日本东京','JP',5,38,553],
    ['us-01','洛杉矶节点','美国洛杉矶','US',4,31,472],
    ['de-01','法兰克福节点','德国法兰克福','DE',3,24,401],
    ['tw-01','台北边缘节点','中国台湾','TW',3,21,368],
    ['hk-02','香港备用节点','中国香港','HK',2,17,355],
    ['sg-02','新加坡备用节点','新加坡','SG',1,13,340]
  ].map(([id,name,region,code,inbounds,clients,total], i) => ({
    id:'demo-'+id,name,url:'https://'+id+'.example.net',region,code,online:i!==4,inbounds,clients,
    up:Math.round(total*gb*.23),down:Math.round(total*gb*.77)
  }));
  const demoActivity = [];
  const state = {demo:false,panels:[],history:[],activity:[],updatedAt:0,page:'lines',error:''};
  const main = document.getElementById('main-content');
  const nav = document.getElementById('primary-nav');
  const modeMenu = document.getElementById('mode-menu');
  const modeButton = document.getElementById('mode-toggle');
  let selectedRange = 24, filter = 'all', chartObserver, requestId = 0, advancedOpen = false;
  let loading = false;
  const ctx = {state,icon,esc,toast,navigate,refresh,setMode,api,render,bytes};

  function toast(message, type='success') {
    const stack = document.getElementById('nx-toast-stack');
    const el = document.createElement('div');
    el.className = 'nx-toast'+(type==='error'?' error':'');
    el.innerHTML = icon(type==='error'?'circle-alert':'circle-check',16)+'<span>'+esc(message)+'</span>';
    stack.append(el);
    setTimeout(()=>el.remove(),4200);
  }
  async function api(path,options={}) {
    const init = {...options,headers:new Headers(options.headers || {})};
    if (init.body && typeof init.body==='object' && !(init.body instanceof FormData) && !(init.body instanceof Blob)) {
      init.headers.set('Content-Type','application/json');
      init.body = JSON.stringify(init.body);
    }
    let response;
    try { response = await fetch(path,init); } catch { throw new Error('无法连接管理服务，请检查网络连接'); }
    let data;
    try { data = await response.json(); } catch { throw new Error('管理服务返回了无法读取的响应（HTTP '+response.status+'）'); }
    if (!response.ok) throw new Error(data?.error || data?.message || '请求失败（HTTP '+response.status+'）');
    return data;
  }
  function bytes(amount, digits=1) {
    const n = Number(amount) || 0;
    const units = ['B','KB','MB','GB','TB','PB'];
    const i = Math.max(0,Math.min(units.length-1,Math.floor(Math.log(Math.max(n,1))/Math.log(1024))));
    return [Number((n/1024**i).toFixed(i ? digits : 0)).toLocaleString('en-US'),units[i]];
  }
  function addr(url) {
    try { return new URL(url).host; } catch { return String(url || ''); }
  }
  function nfmt(n) { return Number(n || 0).toLocaleString('en-US'); }
  function currentPage() {
    const id = location.hash.slice(1);
    return pages.has(id) ? id : 'lines';
  }
  function navigate(page) {
    if (!pages.has(page)) return;
    if (location.hash==='#'+page) { render(); closeSidebar(); return; }
    location.hash = page;
  }
  function closeSidebar() {
    document.getElementById('sidebar').classList.remove('is-open');
    document.getElementById('mobile-mask').hidden = true;
    document.getElementById('mobile-menu').setAttribute('aria-expanded','false');
  }
  function closeModeMenu() {
    modeMenu.hidden=true;
    modeButton.setAttribute('aria-expanded','false');
  }
  async function setMode(mode) {
    const demo = mode==='demo';
    requestId++;
    closeModeMenu();
    if (demo!==state.demo) {
      state.demo=demo;
      state.panels=demo ? demoPanels : [];
      state.activity=demo ? demoActivity : [];
      state.history=[];
      state.updatedAt=demo ? Date.now() : 0;
      state.error='';
      filter='all';
      main.replaceChildren();
    }
    await refresh();
  }
  async function refresh() {
    const id=++requestId;
    if (state.page==='lines') {
      renderShell();
      await window.Lines.mount(main,ctx);
      if (id===requestId) renderShell();
      return;
    }
    if (state.demo) {
      state.updatedAt=Date.now();
      state.error='';
      loading=false;
      render();
      return;
    }
    loading=true;
    if (state.page==='overview' || !main.firstElementChild) render();
    try {
      const result=await api('/api/overview');
      if (id!==requestId) return;
      state.panels=Array.isArray(result.panels) ? result.panels : [];
      state.history=Array.isArray(result.history) ? result.history : [];
      state.updatedAt=Date.now();
      state.error='';
    } catch(error) {
      if (id!==requestId) return;
      state.error=error.message;
      toast(error.message,'error');
    } finally {
      if (id===requestId) { loading=false; render(); }
    }
  }
  function renderShell() {
    document.title=(pages.get(state.page) || '总览')+' · Nexus 面板管理台';
    document.getElementById('crumb-page').textContent=pages.get(state.page);
    nav.innerHTML=groups.map(([group,rows],i)=>{
      const links=rows.map(([id,name,ic])=>'<a class="nx-navitem" href="#'+id+'"'+(state.page===id?' aria-current="page"':'')+'>'+icon(ic,17)+'<span>'+name+'</span></a>').join('');
      return i===0?'<div class="nx-navgroup"><h2>'+group+'</h2>'+links+'</div>':'<details class="nx-navgroup nx-advanced" '+(advancedOpen||rows.some(r=>r[0]===state.page)?'open':'')+'><summary>'+icon('settings-2',17)+group+icon('chevron-down',13)+'</summary>'+links+'</details>';
    }).join('');
    nav.querySelector('details').ontoggle=e=>{advancedOpen=e.currentTarget.open;};
    modeButton.closest('.nx-mode-wrap').hidden=state.page==='lines';
    document.querySelector('.nx-top-divider').hidden=state.page==='lines';
    modeButton.innerHTML=icon(state.demo?'flask-conical':'server',13)+'<span>'+(state.demo?'演示工作区':'我的工作区')+'</span>'+icon('chevron-down',13);
    modeMenu.innerHTML=[['demo','演示工作区','flask-conical'],['live','我的工作区','server']].map(([id,label,ic])=>'<button data-mode="'+id+'">'+icon(ic,15)+label+((id==='demo')===state.demo?icon('check',14):'')+'</button>').join('');
    const stamp=state.updatedAt ? new Date(state.updatedAt).toLocaleTimeString('zh-CN',{hour12:false}) : '';
    const sync=document.getElementById('sync-label');
    sync.classList.toggle('nx-sync-error',Boolean(state.error));
    sync.innerHTML='<i></i>'+(loading?'正在同步':state.error?'同步失败':stamp?'最近同步 '+esc(stamp):'等待同步');
  }
  function render() {
    chartObserver?.disconnect();
    renderShell();
    if (state.page==='lines') {
      window.Lines.mount(main,ctx).then(renderShell);
      return;
    }
    if (state.page==='overview') {
      renderOverview();
      return;
    }
    if (window.Workflows?.render) {
      Promise.resolve(window.Workflows.render(state.page,main,ctx)).catch(error=>toast(error.message,'error'));
    } else {
      main.innerHTML='<div class="nx-empty">管理模块加载失败，请刷新页面。</div>';
    }
  }
  function openFlow(action,payload) {
    if (!window.Workflows?.open) { toast('管理模块加载失败，请刷新页面','error'); return; }
    Promise.resolve(window.Workflows.open(action,ctx,payload)).catch(error=>toast(error.message,'error'));
  }
  function sparkline(values,tone,index) {
    if (!values.length) return '';
    const max=Math.max(...values,1), min=Math.min(...values,0);
    const pts=values.map((v,i)=>[2+76*i/Math.max(values.length-1,1),33-(v-min)/(max-min||1)*30]);
    const d=spline(pts);
    return '<svg class="nx-spark" viewBox="0 0 80 38" aria-hidden="true"><defs><linearGradient id="spark'+index+'" x1="0" x2="0" y1="0" y2="1"><stop stop-color="'+tone+'" stop-opacity=".21"/><stop offset="1" stop-color="'+tone+'" stop-opacity=".015"/></linearGradient></defs><path d="'+d+' L78 37 H2Z" fill="url(#spark'+index+')"/><path d="'+d+'" fill="none" stroke="'+tone+'" stroke-width="1.8" stroke-linecap="round"/></svg>';
  }
  function metric(label,ic,color,wash,value,unit,foot,points,index) {
    return '<article class="nx-metric" style="--tone:'+color+';--wash:'+wash+'"><div class="nx-metric-title"><span class="nx-metric-icon">'+icon(ic,16)+'</span>'+label+'</div><div class="nx-metric-value">'+esc(value)+(unit?'<small>'+esc(unit)+'</small>':'')+'</div><div class="nx-stat-foot">'+foot+'</div>'+sparkline(points,color,index)+'</article>';
  }
  function statCards() {
    const p=state.panels, online=p.filter(x=>x.online).length;
    const totals=k=>p.reduce((a,x)=>a+(Number(x[k])||0),0);
    const [volume,unit]=bytes(totals('up')+totals('down'),2);
    const missing=p.filter(x=>x.inbounds==null).length;
    const scope=missing?'已同步 '+(p.length-missing)+' / '+p.length+' 个面板':'';
    const noData=missing>0 && missing===p.length;
    const trends=state.demo ? [
      [8,8,9,9,10,9,10,11,11,12,13,13], [1,3,2,5,4,7,6,7,6,10,9,12],
      [2,3,3,5,4,6,6,8,7,9,8,12], [2,4,3,6,5,9,6,10,8,11,10,14]
    ] : [[],[],[],state.history.slice(-18).map(h=>Number(h.up)+Number(h.down))];
    return metric('面板总数','server','#7860d6','#f0edfc',nfmt(p.length),'','<i></i><em>'+online+' 个在线</em>',trends[0],0)
      +metric('入站总数','log-in','#4389ca','#eaf3fc',noData?'—':nfmt(totals('inbounds')),'',scope || (state.demo?'VLESS · TCP · REALITY':'已同步面板'),trends[1],1)
      +metric('客户端数','users-round','#3c9a86','#e8f6f0',noData?'—':nfmt(totals('clients')),'',scope || '已配置客户端',trends[2],2)
      +metric('累计流量','activity','#ce8750','#fcf1e7',noData?'—':volume,noData?'':unit,scope || '上行与下行合计',trends[3],3);
  }
  function spline(pts) {
    if (!pts.length) return '';
    let d='M'+pts[0][0].toFixed(2)+' '+pts[0][1].toFixed(2);
    for(let i=1;i<pts.length;i++) {
      const a=pts[i-1],b=pts[i],mid=(a[0]+b[0])/2;
      d+='C'+mid.toFixed(2)+' '+a[1].toFixed(2)+' '+mid.toFixed(2)+' '+b[1].toFixed(2)+' '+b[0].toFixed(2)+' '+b[1].toFixed(2);
    }
    return d;
  }
  const places=[
    [/香港|hong.?kong/i,'HK',114.2,22.3],[/台湾|台北|taipei|taiwan/i,'TW',121.6,25],
    [/新加坡|singapore/i,'SG',103.8,1.35],[/东京|日本|tokyo|japan/i,'JP',139.7,35.7],
    [/洛杉矶|los.?angeles/i,'US',-118.2,34.1],[/法兰克福|德国|frankfurt/i,'DE',8.7,50.1],
    [/伦敦|london/i,'GB',-.13,51.5],[/首尔|韩国|seoul/i,'KR',127,37.6],
    [/悉尼|sydney/i,'AU',151.2,-33.9],[/纽约|new.?york/i,'US',-74,40.7]
  ];
  function lookup(panel) { return places.find(([re])=>re.test(panel.region||'')); }
  function networkCard() {
    const p=state.panels,online=p.filter(x=>x.online).length;
    const grouped=new Map();
    p.forEach(panel=>{
      const loc=lookup(panel);
      if (!loc) return;
      const existing=grouped.get(loc[1]);
      grouped.set(loc[1],{x:loc[2]+180,y:81-loc[3],live:panel.online||existing?.live,title:panel.region});
    });
    const pins=[...grouped.values()].map(({x,y,live,title})=>{
      const c=live?'#a99aff':'#e4b885';
      return '<g><title>'+esc(title)+'</title><circle cx="'+x+'" cy="'+y+'" r="7" fill="'+c+'" opacity=".17"/><circle cx="'+x+'" cy="'+y+'" r="4" fill="'+c+'" opacity=".38"/><circle cx="'+x+'" cy="'+y+'" r="2" fill="#eeeaff"/></g>';
    }).join('');
    return '<section class="nx-network"><div class="nx-section-head"><h2>面板健康</h2>'+icon('globe',18)+'</div><p class="nx-network-sub">'+(grouped.size?'地图已标记 '+grouped.size+' 个地区':'暂无可定位的地区')+'</p><div class="nx-map"><svg viewBox="0 0 360 136" role="img" aria-label="已配置面板地理分布"><image class="nx-map-base" href="/network-map.svg" width="360" height="136"/>'+pins+'</svg></div><div class="nx-health-stats"><div><small>面板在线率</small><strong>'+(p.length?Number((online/p.length*100).toFixed(1))+'<span>%</span>':'—')+'</strong></div><div><small><i></i>在线面板</small><strong>'+online+'</strong></div><div><small><i class="offline"></i>离线面板</small><strong>'+(p.length-online)+'</strong></div></div></section>';
  }
  function panelRows() {
    const list=state.panels.filter(p=>filter==='all'||p.online===(filter==='on'));
    return list.slice(0,5).map(p=>{
      const code=lookup(p)?.[1] || '';
      const volume=p.up!=null&&p.down!=null?bytes(Number(p.up)+Number(p.down)).join(' '):'—';
      return '<tr><td><button class="nx-panel-title" data-detail="'+esc(p.id)+'"><span class="nx-region-code" data-region="'+code+'">'+(code || icon('server',15))+'</span><span><strong>'+esc(p.name)+'</strong><small>'+esc(addr(p.url))+'</small></span></button></td><td><span class="nx-area">'+icon('map-pin',13)+esc(p.region||'未设置')+'</span></td><td><span class="nx-status-tag'+(p.online?'':' offline')+'"><i></i>'+(p.online?'在线':'离线')+'</span></td><td><div class="nx-inventory-counts">'+(p.inbounds==null?'—':nfmt(p.inbounds))+'<span>/</span>'+(p.clients==null?'—':nfmt(p.clients))+'</div></td><td>'+volume+'</td><td><div class="wf-panel-actions"><button class="nx-card-action" data-action="panel-sync" data-panel="'+esc(p.id)+'">同步</button><button class="nx-card-action" data-action="panel-remove" data-panel="'+esc(p.id)+'">删除</button></div></td></tr>';
    }).join('') || '<tr><td colspan="6"><div class="nx-empty">'+icon('server',24)+(loading?'正在同步面板…':state.panels.length?'没有符合筛选条件的面板':'尚未接入面板')+(!loading&&!state.panels.length?'<br><button class="nx-button" data-action="add-panel">'+icon('plus',14)+'接入面板</button>':'')+'</div></td></tr>';
  }
  function inventory() {
    return '<section class="nx-surface nx-table"><div class="nx-table-head"><div class="nx-table-title"><h2>面板列表</h2><span class="nx-number">'+state.panels.length+'</span></div><div class="nx-table-actions"><label class="nx-period"><select id="nx-status-filter" aria-label="按连接状态筛选"><option value="all">全部状态</option><option value="on">在线</option><option value="off">离线</option></select>'+icon('chevron-down',12)+'</label><button class="nx-card-action" data-nav="panels">查看全部'+icon('arrow-right',13)+'</button></div></div><div class="nx-table-scroll"><table><thead><tr><th>面板名称</th><th>地区</th><th>状态</th><th>入站 / 客户端</th><th>累计流量</th><th aria-label="操作"></th></tr></thead><tbody>'+panelRows()+'</tbody></table></div></section>';
  }
  function renderOverview() {
    const today=new Date();
    const date=today.toLocaleDateString('zh-CN',{year:'numeric',month:'long',day:'numeric'})+' · '+today.toLocaleDateString('zh-CN',{weekday:'long'});
    main.innerHTML='<div class="nx-heading"><div><h1>工作台概览</h1><p class="nx-heading-meta">'+icon('calendar-days',14)+esc(date)+'</p></div><div class="nx-heading-actions"><button class="nx-button nx-secondary" data-action="refresh" title="刷新数据" aria-label="刷新数据" '+(loading?'disabled':'')+'>'+icon('refresh-cw',15)+'<span>刷新</span></button><button class="nx-button nx-primary" data-action="batch">'+icon('layers',16)+'批量操作</button></div></div>'
      +(state.error?'<div class="nx-error-banner">'+icon('circle-alert',16)+esc(state.error)+'</div>':'')
      +'<div class="nx-metrics">'+statCards()+'</div><div class="nx-panorama"><section class="nx-surface nx-chart-card"><div class="nx-section-head"><h2>流量趋势</h2><label class="nx-period"><select id="nx-range" aria-label="流量统计时间范围"><option value="24">近 24 小时</option><option value="168">近 7 天</option><option value="720">近 30 天</option></select>'+icon('chevron-down',12)+'</label></div><div class="nx-legend"><span><i class="nx-download-dot"></i>下行流量</span><span><i class="nx-upload-dot"></i>上行流量</span></div><div class="nx-plot" id="nx-plot"></div></section>'+networkCard()+'</div>'+inventory();
    document.getElementById('nx-status-filter').value=filter;
    document.getElementById('nx-range').value=String(selectedRange);
    const plot=document.getElementById('nx-plot');
    renderChart(plot);
    chartObserver=new ResizeObserver(()=>renderChart(plot));
    chartObserver.observe(plot);
  }
  function chartSeries() {
    const now=state.updatedAt||Date.now();
    const end=state.demo?Math.floor(now/hour)*hour:now;
    const start=end-selectedRange*hour;
    if (!state.demo) {
      return state.history.filter(s=>Number.isFinite(Number(s.ts))&&Number(s.ts)>=start&&Number(s.ts)<=end)
        .map(s=>({ts:Number(s.ts),up:Number(s.up)||0,down:Number(s.down)||0})).sort((a,b)=>a.ts-b.ts);
    }
    const baseline=[.24,.28,.25,.31,.27,.45,.34,.37,.61,.55,.58,.39,.44,.71,.64,.74,.58,.62,.49,.57,.78,.7,.88,.65,.72,.59,.67,.50,.56,.76,.68,.75,.63,.72,.86,.79,.91,.78,.70,.82,.67,.71,.64,.60,.74,.64,.71,.84];
    const count=selectedRange===24?baseline.length:selectedRange===168?28:30;
    return Array.from({length:count},(_,i)=>{
      const d=baseline[(i+(selectedRange===24?0:7))%baseline.length],scale=selectedRange===24?72:selectedRange===168?230:330;
      return {ts:start+(end-start)*i/(count-1),down:d*scale*gb,up:(d*.43+.05+(Math.sin(i*1.8)*.06))*scale*gb};
    });
  }
  function renderChart(plot) {
    if (!plot.isConnected) return;
    const data=chartSeries();
    if (!data.length) {
      plot.innerHTML='<div class="nx-chart-empty">'+icon('chart-line',25)+(loading?'正在读取流量采样…':'当前时间范围暂无流量采样')+'</div>';
      return;
    }
    const w=plot.clientWidth||510,h=plot.clientHeight||200,left=42,right=w-9,top=16,bottom=h-26;
    const rawMax=Math.max(...data.map(p=>Math.max(p.up,p.down)),1);
    const unit=bytes(rawMax)[1],divisor=1024**['B','KB','MB','GB','TB','PB'].indexOf(unit);
    const rough=rawMax/divisor/4,stepBase=10**Math.floor(Math.log10(rough));
    const step=[1,2,2.5,5,10].find(v=>v*stepBase>=rough)*stepBase;
    const max=4*step*divisor;
    const end=state.demo?data[data.length-1].ts:(state.updatedAt||Date.now()),start=end-selectedRange*hour;
    const x=p=>left+(p.ts-start)/(end-start)*(right-left),y=v=>bottom-(v/max)*(bottom-top);
    // The server returns hourly buckets. Leave gaps where the collectors were offline.
    const segments=[];
    data.forEach((point,index)=>{
      if (!index || (!state.demo && point.ts-data[index-1].ts>hour*1.5)) segments.push([]);
      segments[segments.length-1].push(point);
    });
    const curves=segments.map(points=>{
      const base=' L'+x(points[points.length-1])+' '+bottom+' L'+x(points[0])+' '+bottom+'Z';
      return {points,base,down:spline(points.map(p=>[x(p),y(p.down)])),up:spline(points.map(p=>[x(p),y(p.up)]))};
    });
    let grid='';
    for(let i=0;i<5;i++){
      const val=i*step,py=y(val*divisor);
      grid+='<path d="M'+left+' '+py+'H'+right+'" stroke="#e8e9f1" stroke-dasharray="3 4"/><text text-anchor="end" x="'+(left-7)+'" y="'+(py+3)+'">'+Number(val.toPrecision(3))+' '+unit+'</text>';
    }
    const tickCount=w<370?4:6;
    const formatTick=ts=>new Date(ts).toLocaleString('zh-CN',selectedRange===24?{hour:'2-digit',minute:'2-digit',hour12:false}:{month:'2-digit',day:'2-digit'});
    for(let i=0;i<=tickCount;i++){
      const f=i/tickCount;
      grid+='<text x="'+(left+(right-left)*f)+'" y="'+(h-5)+'" text-anchor="'+(i===0?'start':i===tickCount?'end':'middle')+'">'+esc(formatTick(start+(end-start)*f))+'</text>';
    }
    const plotted=curves.map(({points,base,down,up})=>{
      if(points.length===1) {
        const p=points[0];
        return '<circle cx="'+x(p)+'" cy="'+y(p.down)+'" r="3" fill="#7055ce"/><circle cx="'+x(p)+'" cy="'+y(p.up)+'" r="3" fill="#a99add"/>';
      }
      return '<path d="'+down+base+'" fill="url(#traffic-fill)"/><path d="'+up+base+'" fill="url(#upload-fill)"/><path d="'+down+'" fill="none" stroke="#775bd2" stroke-width="2.6" stroke-linecap="round"/><path d="'+up+'" fill="none" stroke="#ad9cde" stroke-width="2.1" stroke-linecap="round"/>';
    }).join('');
    plot.innerHTML='<svg viewBox="0 0 '+w+' '+h+'" role="img" aria-label="上行与下行流量随时间变化"><defs><linearGradient id="traffic-fill" x1="0" x2="0" y1="0" y2="1"><stop stop-color="#8d75e7" stop-opacity=".26"/><stop offset="1" stop-color="#a998e9" stop-opacity=".035"/></linearGradient><linearGradient id="upload-fill" x1="0" x2="0" y1="0" y2="1"><stop stop-color="#c1b2ed" stop-opacity=".23"/><stop offset="1" stop-color="#d9d1ed" stop-opacity=".03"/></linearGradient></defs>'+grid+plotted+'<g id="nx-mark" visibility="hidden"><line y1="'+top+'" y2="'+bottom+'" stroke="#a99dc9" stroke-dasharray="3 3"/><circle r="4" fill="#795ad8" stroke="white" stroke-width="1.5"/><circle r="3" fill="#a995da" stroke="white" stroke-width="1.5"/></g></svg><div class="nx-tooltip" hidden></div>';
    const tip=plot.querySelector('.nx-tooltip'),mark=plot.querySelector('#nx-mark');
    function show(event) {
      const px=event.clientX-plot.getBoundingClientRect().left;
      const i=data.reduce((best,p,k)=>Math.abs(x(p)-px)<Math.abs(x(data[best])-px)?k:best,0);
      const p=data[i],cx=x(p);
      mark.setAttribute('visibility','visible');
      const [line,a,b]=mark.children;
      line.setAttribute('x1',cx);line.setAttribute('x2',cx);
      a.setAttribute('cx',cx);a.setAttribute('cy',y(p.down));
      b.setAttribute('cx',cx);b.setAttribute('cy',y(p.up));
      tip.innerHTML='<strong>'+esc(new Date(p.ts).toLocaleString('zh-CN',{month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}))+'</strong><span><i class="nx-download-dot"></i>下行<b>'+bytes(p.down).join(' ')+'</b></span><span><i class="nx-upload-dot"></i>上行<b>'+bytes(p.up).join(' ')+'</b></span>';
      tip.hidden=false;
      tip.style.left=Math.max(0,Math.min(cx+9,w-tip.offsetWidth-2))+'px';
      tip.style.top='13px';
    }
    plot.onpointermove=show;
    plot.onpointerdown=show;
    plot.onpointerleave=()=>{mark.setAttribute('visibility','hidden');tip.hidden=true;};
  }
  main.addEventListener('click',async ev=>{
    const button=ev.target.closest('[data-action],[data-detail],[data-nav]');
    if (!button||state.page!=='overview') return;
    if (button.dataset.nav) navigate(button.dataset.nav);
    else if (button.dataset.detail) openFlow('panel-detail',{id:button.dataset.detail});
    else if (button.dataset.action==='refresh') { await refresh(); if (!state.error) toast('数据已刷新'); }
    else openFlow(button.dataset.action,button.dataset.panel?{panelId:button.dataset.panel}:{});
  });
  main.addEventListener('change',ev=>{
    if (ev.target.id==='nx-range') { selectedRange=Number(ev.target.value); renderChart(document.getElementById('nx-plot')); }
    if (ev.target.id==='nx-status-filter') { filter=ev.target.value; main.querySelector('.nx-table tbody').innerHTML=panelRows(); }
  });
  modeButton.addEventListener('click',()=>{
    modeMenu.hidden=!modeMenu.hidden;
    modeButton.setAttribute('aria-expanded',String(!modeMenu.hidden));
  });
  modeMenu.addEventListener('click',ev=>{const b=ev.target.closest('[data-mode]');if(b) setMode(b.dataset.mode);});
  document.addEventListener('click',ev=>{if(!ev.target.closest('.nx-mode-wrap')) closeModeMenu();});
  document.addEventListener('keydown',ev=>{if(ev.key==='Escape'){closeModeMenu();closeSidebar();}});
  document.getElementById('mobile-mask').addEventListener('click',closeSidebar);
  document.getElementById('mobile-menu').innerHTML=icon('menu',18);
  document.getElementById('crumb-icon').innerHTML=icon('panels-top-left',16);
  document.getElementById('mobile-menu').addEventListener('click',()=>{
    document.getElementById('sidebar').classList.add('is-open');
    document.getElementById('mobile-mask').hidden=false;
    document.getElementById('mobile-menu').setAttribute('aria-expanded','true');
  });
  nav.addEventListener('click',ev=>{if(ev.target.closest('a'))closeSidebar();});
  window.addEventListener('hashchange',()=>{
    state.page=currentPage();closeSidebar();closeModeMenu();window.scrollTo(0,0);
    if(state.page==='lines') {setMode('live');return;}
    main.replaceChildren();refresh();
  });
  state.page=currentPage();
  refresh();
})();
