(() => {
  'use strict';
  const asList = value => Array.isArray(value) ? value : [];
  const targetKey = target => JSON.stringify([target.panelId, target.clientId, target.email, target.uuid]);
  const noteOf = client => String(client.note ?? client.comment ?? '');
  const durationOptions = [1, 2, 3, 6, 12];
  const exportColumns = [
    ['name','节点名称'], ['link','VLESS 链接'], ['expiry','到期时间'], ['qr','二维码'],
    ['panel','所属面板'], ['inbound','关联入站'], ['note','备注'], ['host','服务器地址'], ['port','端口'], ['status','客户端状态']
  ];
  const dayMs = 86400000;
  const day = time => Number(time) > 0 ? new Date(Number(time)).toLocaleString('zh-CN',{hour12:false})
    : Number(time) < 0 ? `首次使用后 ${Math.abs(time) / 86400000} 天` : '永久有效';

  function exportExpiry(time) {
    if (!(Number(time) > 0)) return day(time);
    const date = new Date(Number(time));
    const pad = n => String(n).padStart(2,'0');
    return `${date.getFullYear()}-${pad(date.getMonth()+1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  }

  function demoExpiry(current, term) {
    const now = Date.now();
    const base = Math.max(current, now);
    let next;
    if (term.days) {
      next = base + term.days * dayMs;
    } else {
      const date = new Date(base);
      const originalDay = date.getUTCDate();
      date.setUTCDate(1);
      date.setUTCMonth(date.getUTCMonth() + term.months + 1, 0);
      date.setUTCDate(Math.min(originalDay, date.getUTCDate()));
      next = date.getTime();
    }
    return current < 0 ? current - (next - base) : next;
  }

  async function panelClients(ctx, wf, panel) {
    let clients, inbounds, links, warnings;
    if (ctx.state.demo) {
      if (panel.online === false) throw new Error('演示面板离线');
      inbounds = wf.demoData(panel).inbounds;
      clients = inbounds.flatMap(i => i.settings.clients.map(c => ({...c, uuid:c.id, inboundIds:[i.id]})));
      links = []; warnings = [];
    } else {
      const result = await ctx.api(`/api/panels/${encodeURIComponent(panel.id)}/client-links`);
      ({clients, inbounds, links, warnings} = result);
    }
    const inboundMap = new Map(asList(inbounds).map(i => [String(i.id), i]));
    const shares = new Map();
    for (const link of asList(links)) {
      if (!link.link?.startsWith('vless://')) continue;
      const key = String(link.clientId);
      if (!shares.has(key)) shares.set(key, []);
      shares.get(key).push(link);
    }
    const rows = asList(clients).map(client => {
      const target = {panelId:panel.id, clientId:client.id, email:client.email, uuid:client.uuid || ''};
      const bindings = asList(client.inboundIds).map(id => inboundMap.get(String(id)) || {id});
      const options = (shares.get(String(client.id)) || []).filter(l =>
        l.email === target.email && l.uuid === target.uuid && bindings.some(i => String(i.id) === String(l.inboundId)));
      return {panel, client, bindings, options, target};
    });
    return {rows, warnings:asList(warnings)};
  }

  async function render(host, ctx, wf) {
    const {esc, icon} = ctx;
    const root = document.createElement('div');
    root.className = 'wf-page wf-clients';
    root.innerHTML = `<div class="wf-heading"><div><h1>客户端管理</h1><p>${ctx.state.demo?'演示工作区':'我的工作区'} · ${ctx.state.panels.length} 个面板</p></div>
      <div class="wf-actions"><button class="wf-button" data-refresh-clients>${icon('refresh-cw')}刷新</button>
      <button class="wf-button" data-expiry>${icon('calendar-days')}批量改期</button>
      <button class="wf-button wf-primary" data-create>${icon('plus')}批量添加客户端</button></div></div>
      <div data-warnings class="wf-banners" role="status"></div><div data-pending></div>
      <section class="wf-surface"><div class="wf-subhead"><div class="wf-row"><h2>客户端列表</h2><span class="wf-count" data-count></span></div>
      <div class="wf-actions wf-client-bulk"><button class="wf-button" data-copy-selected disabled>${icon('copy',15)}复制选中链接</button>
      <button class="wf-button" data-export-selected disabled>${icon('download',15)}导出 Excel</button><button class="wf-button" data-notes-selected disabled>${icon('pencil',15)}修改备注</button>
      <button class="wf-button" data-extend-selected disabled>${icon('calendar-days',15)}增加时间</button>
      <button class="wf-button" data-quota-selected disabled>${icon('pencil',15)}修改流量上限</button>
      <button class="wf-button" data-reset-selected disabled>${icon('rotate-ccw',15)}重置已用流量</button>
      <button class="wf-button wf-danger-link" data-delete-selected disabled>删除选中</button></div></div>
      <div class="wf-toolbar"><label class="wf-search">${icon('search',15)}<input type="search" placeholder="搜索客户端、入站或备注" aria-label="搜索客户端、入站或备注"></label>
      <select data-filter class="wf-select" aria-label="筛选面板"><option value="">全部面板</option>${ctx.state.panels.map(p=>`<option value="${esc(p.id)}">${esc(p.name)}</option>`).join('')}</select></div>
      <div data-list class="wf-table-wrap"><div class="wf-loading">正在读取客户端…</div></div><div data-pagination></div></section>`;
    host.replaceChildren(root);
    const rows = [], selected = new Map(), chosen = new Map(), pending = [], warnings = [];
    const savingStatus = new Set();
    let page = 0;
    const table = root.querySelector('[data-list]'), batch = root.querySelector('[data-delete-selected]');
    const label = i => i.remark || i.tag || `入站 #${i.id}`;
    const panelName = t => ctx.state.panels.find(p=>p.id===t.panelId)?.name || t.panelId;
    const quotaText = client => client.totalGB == null ? '—' : client.totalGB === 0 ? '不限' : ctx.bytes(client.totalGB,2).join(' ');
    function usageText(client) {
      const {up,down} = client.traffic || {};
      return up == null || down == null ? '—' : ctx.bytes(Number(up) + Number(down),2).join(' ');
    }
    const selectedLink = row => row.options.find(o=>String(o.inboundId)===chosen.get(targetKey(row.target))) || row.options[0];
    root.querySelector('[data-refresh-clients]').onclick = () => ctx.refresh();
    root.querySelector('[data-create]').onclick = () => wf.open('client.create',ctx);
    root.querySelector('[data-expiry]').onclick = () => wf.open('client.expiry',ctx);
    function shareControls(row) {
      if (!row.options.length) return '<span class="wf-meta">'+(ctx.state.demo?'演示客户端':row.bindings.length?'无可用 VLESS TCP Reality 链接':'未绑定入站')+'</span>';
      const select = row.options.length > 1 ? `<select data-share-inbound class="wf-select" aria-label="选择要分享的入站">${row.options.map(o=>{
        const inbound=row.bindings.find(i=>String(i.id)===String(o.inboundId));
        return `<option value="${esc(o.inboundId)}" ${o===selectedLink(row)?'selected':''}>${esc(label(inbound))} · #${esc(o.inboundId)}</option>`;
      }).join('')}</select>` : '';
      return `<div class="wf-client-share">${select}<div class="wf-panel-actions"><button class="wf-link" data-copy-link>复制链接</button><button class="wf-link" data-show-qr>二维码</button><button class="wf-link" data-row-export>导出</button></div></div>`;
    }
    function statusControl(row) {
      const enabled = row.client.enable;
      if (typeof enabled !== 'boolean') return '<span class="wf-meta">状态未知</span>';
      const busy = savingStatus.has(targetKey(row.target));
      return `<button type="button" class="wf-client-switch" data-row-status role="switch" aria-label="启用客户端 ${esc(row.client.email)}" aria-checked="${enabled}" ${busy?'disabled aria-busy="true"':''}><span aria-hidden="true"></span>${busy?'保存中':enabled?'已启用':'已停用'}</button>`;
    }
    function draw() {
      if (!root.isConnected) return;
      const query=root.querySelector('[type=search]').value.trim().toLowerCase(), panel=root.querySelector('[data-filter]').value;
      const filtered=rows.map((row,index)=>({row,index})).filter(({row})=>
        (!panel || row.panel.id===panel) && (!query || [row.client.email,row.panel.name,noteOf(row.client),...row.bindings.map(label)].some(s=>String(s).toLowerCase().includes(query))));
      const pages=Math.max(1,Math.ceil(filtered.length/12));page=Math.min(page,pages-1);
      const shown=filtered.slice(page*12,(page+1)*12);
      batch.disabled=!selected.size;
      root.querySelector('[data-copy-selected]').disabled=!selected.size;
      root.querySelector('[data-export-selected]').disabled=!selected.size;
      root.querySelector('[data-notes-selected]').disabled=!selected.size;
      root.querySelector('[data-extend-selected]').disabled=!selected.size;
      root.querySelector('[data-quota-selected]').disabled=!selected.size;
      root.querySelector('[data-reset-selected]').disabled=!selected.size;
      batch.textContent=selected.size?`删除选中（${selected.size}）`:'删除选中';
      root.querySelector('[data-count]').textContent=String(filtered.length);
      table.innerHTML=shown.length?`<table class="wf-table"><thead><tr><th><input aria-label="选择本页客户端" data-select-page type="checkbox"></th><th>客户端 / 所属面板</th><th>状态</th><th>关联入站</th><th>到期时间</th><th>已用 / 流量上限</th><th>分享</th><th>操作</th><th>备注</th></tr></thead><tbody>${shown.map(({row,index})=>{
        const {client,bindings}=row;
        return `<tr data-row="${index}"><td><input data-select-client type="checkbox" aria-label="选择 ${esc(client.email)}" ${selected.has(targetKey(row.target))?'checked':''}></td>
          <td><strong>${esc(client.email)}</strong><small class="wf-meta">${esc(row.panel.name)}</small></td>
          <td data-status-cell>${statusControl(row)}</td>
          <td class="wf-binding-list">${bindings.map(i=>`<span>${esc(label(i))} · #${esc(i.id)}</span>`).join('')||'未绑定'}</td>
          <td>${esc(day(client.expiryTime))}</td><td><div class="wf-traffic-cell"><span>${esc(usageText(client))} / ${esc(quotaText(client))}</span><div class="wf-panel-actions"><button class="wf-link" data-row-quota>改上限</button><button class="wf-link" data-row-reset>重置流量</button></div></div></td><td>${shareControls(row)}</td>
          <td><div class="wf-panel-actions"><button class="wf-link" data-row-extend>增加时间</button>${bindings.length?'<button class="wf-link" data-row-expiry>改期</button>':''}<button class="wf-link wf-danger-link" data-remove-client>删除</button></div></td>
          <td class="wf-client-note"><button class="wf-note-editor" data-row-note title="${esc(noteOf(client) || '添加备注')}" aria-label="修改 ${esc(client.email)} 的备注"><span>${esc(noteOf(client)) || '添加备注'}</span>${icon('pencil',14)}</button></td></tr>`;
      }).join('')}</tbody></table>`:`<div class="wf-empty">${query||panel?'没有匹配的客户端':warnings.length?'暂无可显示的客户端，请查看读取结果':'暂无客户端'}</div>`;
      const all=table.querySelector('[data-select-page]');
      if(all){
        const n=shown.filter(({row})=>selected.has(targetKey(row.target))).length;
        all.checked=n===shown.length;all.indeterminate=n>0 && !all.checked;
        all.onchange=()=>{for(const {row} of shown){select(row,all.checked);}draw();};
      }
      const pager=root.querySelector('[data-pagination]');
      pager.innerHTML=pages>1?`<div class="wf-pagination"><span>共 ${filtered.length} 个客户端</span><div class="wf-row"><button class="wf-icon-button" data-prev ${page?'':'disabled'} aria-label="上一页">${icon('chevron-left')}</button><span>${page+1} / ${pages}</span><button class="wf-icon-button" data-next ${page===pages-1?'disabled':''} aria-label="下一页">${icon('chevron-right')}</button></div></div>`:'';
      if(pages>1){pager.querySelector('[data-prev]').onclick=()=>{page--;draw();};pager.querySelector('[data-next]').onclick=()=>{page++;draw();};}
    }
    function select(row,on) {
      const key=targetKey(row.target);
      if(on)selected.set(key,row);else selected.delete(key);
    }
    function shareSelection(entries) {
      const nodes = [];
      for (const row of entries) {
        const item = selectedLink(row);
        if (!item) continue;
        const inbound = row.bindings.find(i => String(i.id) === String(item.inboundId));
        const uri = new URL(item.link);
        nodes.push({name:item.name || row.client.email, panel:row.panel.name, inbound:label(inbound),
          expiry:exportExpiry(row.client.expiryTime), note:noteOf(row.client), link:item.link, qr:item.link,
          host:uri.hostname, port:uri.port, status:row.client.enable === false ? '停用' : '启用'});
      }
      const skipped = entries.length - nodes.length;
      return {nodes, summary:`${nodes.length} 个节点${skipped ? `，${skipped} 个客户端无可用链接` : ''}`};
    }
    async function copyLinks(entries) {
      const {nodes, summary} = shareSelection(entries);
      if (!nodes.length) return ctx.toast(summary);
      const text = nodes.map(node => node.link).join('\n');
      try {
        await navigator.clipboard.writeText(text);
        ctx.toast(`已复制 ${summary}`);
      } catch {
        const dialog = wf.modal(ctx,'手动复制链接','copy',`<p>${esc(summary)}。选中下方链接后复制。</p>
          <label class="wf-field"><span>VLESS 链接</span><textarea class="wf-copy-links" readonly></textarea></label>`,
          '<button type="button" class="wf-button" data-select-links>全选链接</button><button type="button" class="wf-button wf-primary" data-wf-close>完成</button>');
        const input = dialog.querySelector('textarea');
        input.value = text;
        const selectText = () => { input.focus(); input.select(); };
        dialog.querySelector('[data-select-links]').onclick = selectText;
        selectText();
      }
    }
    function exportNodes(entries) {
      const {nodes, summary} = shareSelection(entries);
      if (!nodes.length) return ctx.toast(summary);
      const fields = exportColumns.map(([key,title], i) => `<label class="wf-check"><input type="checkbox" name="column" value="${key}" ${i < 4 ? 'checked' : ''}>${title}</label>`).join('');
      const dialog = wf.modal(ctx,'导出 Excel','download',`<form id="wf-form" class="wf-form">
        <p>${esc(summary)}</p><fieldset class="wf-export-fields"><legend>导出字段</legend><div>${fields}</div></fieldset>
        <section><h3 class="wf-preview-heading">内容预览</h3><div class="wf-export-preview" data-preview></div></section>
        <div class="wf-inline-error" role="alert"></div></form>`,wf.submitFooter('下载 Excel'));
      dialog.classList.add('wf-export-dialog');
      const form = dialog.querySelector('form');
      const checked = () => Array.from(form.querySelectorAll('[name=column]:checked'), input => input.value);
      const submit = dialog.querySelector('[data-wf-submit]');
      function preview() {
        const columns = checked();
        submit.disabled = !columns.length;
        const head = exportColumns.filter(([id]) => columns.includes(id)).map(([,title]) => `<th>${title}</th>`).join('');
        const body = nodes.slice(0,5).map(node => `<tr>${columns.map(col => col === 'qr'
          ? '<td>导出时嵌入</td>' : `<td title="${esc(node[col])}">${esc(col === 'expiry' ? node[col].replace('T',' ') : node[col])}</td>`).join('')}</tr>`).join('');
        form.querySelector('[data-preview]').innerHTML = columns.length
          ? `<table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>${nodes.length > 5 ? `<p>显示前 5 个，共 ${nodes.length} 个节点</p>` : ''}`
          : '<p>请选择至少一个字段。</p>';
      }
      form.onchange = preview;
      preview();
      wf.bindForm(dialog, async () => {
        const columns = checked();
        if (!columns.length) throw new Error('请选择至少一个字段');
        const rows = nodes.map(node => Object.fromEntries(columns.map(id => [id,node[id]])));
        const response = await fetch('/api/clients/export-xlsx', {method:'POST',credentials:'same-origin',
          headers:{'Content-Type':'application/json'},body:JSON.stringify({columns,rows})});
        if (!response.ok) {
          const problem = await response.json().catch(() => ({}));
          throw new Error(problem.error || `下载失败（HTTP ${response.status}）`);
        }
        const file = URL.createObjectURL(await response.blob());
        const date = new Date();
        const a = document.createElement('a');
        a.href = file;
        a.download = `节点列表-${date.getFullYear()}年${date.getMonth()+1}月${date.getDate()}日.xlsx`;
        document.body.append(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(file),60000);
        dialog.close();
        ctx.toast(`已导出 ${nodes.length} 个节点`);
      });
    }
    async function editClients(action, targets, change) {
      if (!ctx.state.demo) {
        const {results} = await ctx.api(`/api/clients/${action}`, {method:'POST', body:{targets, ...change}});
        return results;
      }
      return targets.map(target => {
        const panel = ctx.state.panels.find(p => p.id === target.panelId);
        const client = wf.demoData(panel).inbounds.flatMap(i => i.settings.clients)
          .find(c => c.id === target.clientId && c.email === target.email);
        const result = {...target, name:panel.name, ok:false};
        if (!client) return {...result, message:'客户端已移除'};
        if (action === 'status') {
          client.enable = change.enable;
          return {...result, ok:true, enable:client.enable, message:client.enable?'客户端已启用':'客户端已停用'};
        }
        if (action === 'quota') {
          client.totalGB = change.quotaBytes;
          return {...result, ok:true, quotaBytes:change.quotaBytes, message:'已更新客户端流量上限'};
        }
        if (action === 'reset-traffic') {
          client.traffic = {...client.traffic, up:0, down:0};
          client.enable = true;
          return {...result, ok:true, message:'已重置当前周期流量'};
        }
        if (action === 'notes') {
          if (noteOf(client) !== target.note) return {...result, conflict:true, message:'备注已变化，请重新读取'};
          client.comment = change.note;
          return {...result, ok:true, note:change.note, message:'备注已保存'};
        }
        if (Number(client.expiryTime || 0) !== target.expiryTime)
          return {...result, conflict:true, message:'到期时间已变化，请重新读取'};
        const expiryTime = demoExpiry(target.expiryTime, change);
        if (!Number.isFinite(new Date(expiryTime).getTime()))
          return {...result, message:'到期时间超出可表示的日期范围'};
        client.expiryTime = expiryTime;
        return {...result, ok:true, expiryTime:client.expiryTime, message:`新的有效期为 ${day(client.expiryTime)}`};
      });
    }
    async function changeStatus(row, index) {
      const key = targetKey(row.target);
      if (savingStatus.has(key) || typeof row.client.enable !== 'boolean') return;
      const enable = !row.client.enable;
      const cell = () => table.querySelector(`[data-row="${index}"] [data-status-cell]`);
      const button = cell()?.querySelector('button');
      const hadFocus = button && document.activeElement === button;
      savingStatus.add(key);
      if (cell()) cell().innerHTML = statusControl(row);
      try {
        const [result] = await editClients('status', [row.target], {enable});
        if (!result?.ok || result.enable !== enable) throw new Error(result?.message || '面板未确认客户端状态');
        row.client.enable = result.enable;
        if (Number.isSafeInteger(result.updatedAt)) row.client.updatedAt = result.updatedAt;
        ctx.toast(result.message);
      } catch (err) {
        row.client.enable = null;
        ctx.toast(err.message, 'error');
        try {
          const fresh = await panelClients(ctx, wf, row.panel);
          const current = fresh.rows.find(r => targetKey(r.target) === key);
          if (current) Object.assign(row, current);
        } catch {
          ctx.toast('无法确认客户端当前状态，请刷新列表', 'error');
        }
      } finally {
        savingStatus.delete(key);
        const slot = cell();
        if (slot) {
          slot.innerHTML = statusControl(row);
          if (hadFocus && document.activeElement === document.body) slot.querySelector('button')?.focus({preventScroll:true});
        }
      }
    }
    function trafficDialog(action, entries) {
      if (!entries.length) return;
      if (entries.length > 512) return ctx.toast('每次最多可处理 512 个客户端','error');
      const reset = action === 'reset-traffic';
      const title = reset ? '重置已用流量' : '修改流量上限';
      const scope = entries.map(({panel,client}) => `<li><strong>${esc(client.email)}</strong><small>${esc(panel.name)} · 已用 ${esc(usageText(client))} / 上限 ${esc(quotaText(client))}</small></li>`).join('');
      const list = `<ul class="wf-delete-targets wf-edit-targets">${scope}</ul>`;
      const summary = entries.length === 1 ? list : `<details class="wf-client-scope"><summary>已选 ${entries.length} 个客户端 · 查看名单</summary>${list}</details>`;
      const control = reset
        ? '<p>将清零所选客户端在所有绑定入站的本周期上传、下载用量。3x-ui 会尝试重新启用已停用的客户端。</p>'
        : window.Lines.quotaField({required:true});
      const dialog = wf.modal(ctx,title,reset ? 'rotate-ccw' : 'pencil',
        `<form id="wf-form" class="wf-form">${summary}${control}<div class="wf-inline-error" role="alert"></div></form>`,
        wf.submitFooter(reset ? '确认清零' : '保存流量上限'));
      const readQuota = reset ? null : window.Lines.bindQuota(dialog.querySelector('form'));
      if (reset) dialog.querySelector('[data-wf-submit]').classList.add('wf-danger');
      wf.bindForm(dialog,async () => {
        const change = reset ? {} : {quotaBytes:readQuota()};
        const targets = entries.map(({target,client}) => reset ? target : {...target,quotaBytes:client.totalGB});
        let results;
        try {
          results = await editClients(action,targets,change);
        } catch (err) {
          // A transport failure can occur after 3x-ui applied a reset.
          dialog.close();
          ctx.toast(`${err.message}。请核对刷新后的流量再操作。`,'error');
          await ctx.refresh();
          return;
        }
        dialog.close();
        showResults(`${title}结果`,results);
        await ctx.refresh();
      });
    }
    function editDialog(action, entries, draft = {}, originalTargets = new Map()) {
      if (!entries.length) return;
      const notes = action === 'notes';
      const title = notes ? '修改备注' : '增加时间';
      const targets = entries.map(({target, client}) => originalTargets.get(targetKey(target)) ||
        (notes ? {...target, note:noteOf(client)} :
          {...target, expiryTime:Number(client.expiryTime || 0), updatedAt:client.updatedAt}));
      const descriptions = entries.map(({client,panel}) => `<li><strong>${esc(client.email)}</strong>
        <small>${esc(panel.name)} · ${esc(notes ? noteOf(client) || '暂无备注' : day(client.expiryTime))}</small></li>`).join('');
      const list = `<ul class="wf-delete-targets wf-edit-targets">${descriptions}</ul>`;
      const summary = entries.length === 1 ? list :
        `<details class="wf-client-scope"><summary>已选 ${entries.length} 个客户端 · 查看名单</summary>${list}</details>`;
      const common = noteOf(entries[0].client);
      const note = draft.note ?? (entries.every(r => noteOf(r.client) === common) ? common : '');
      const periods = [...durationOptions, 'custom'].map(value => {
        const label = value === 'custom' ? '自定义天数' : value === 12 ? '1 年' : `${value} 个月`;
        const checked = value === (draft.days ? 'custom' : draft.months || 1);
        return `<label><input type="radio" name="duration" value="${value}" ${checked?'checked':''}><span>${label}</span></label>`;
      }).join('');
      const control = notes ? `<label class="wf-field"><span>备注内容</span><textarea name="note" rows="3" placeholder="填写备注">${esc(note)}</textarea>
        <small>${entries.length > 1 ? '将统一更新所选客户端的备注。' : ''}留空保存即可清除。</small></label>` :
        `<fieldset class="wf-client-periods"><legend>增加时长</legend><div>${periods}</div></fieldset>
        <label class="wf-field" data-custom-days><span>增加天数</span><input type="number" name="days" min="1" step="1" inputmode="numeric" placeholder="输入正整数" value="${esc(draft.days ?? '')}"></label>
        <p class="wf-hint">有效客户端从原到期日顺延，已到期和永久有效客户端从现在起算。待首次使用的客户端会延长使用时长。</p>`;
      const retryHint = originalTargets.size ? '<p class="wf-note">部分操作的写入结果待确认，将使用上次提交的内容重试。</p>' : '';
      const dialog = wf.modal(ctx, title, notes ? 'pencil' : 'calendar-days',
        `<form id="wf-form" class="wf-form">${retryHint}${summary}${control}<div class="wf-inline-error" role="alert"></div></form>`,
        wf.submitFooter(notes ? '保存备注' : '确认增加'));
      const form = dialog.querySelector('form');
      if (!notes) {
        const custom = form.querySelector('[data-custom-days]');
        const updateDays = () => {
          const active = form.elements.duration.value === 'custom';
          custom.hidden = !active;
          form.elements.days.disabled = !active;
          form.elements.days.required = active;
        };
        form.addEventListener('change', updateDays);
        updateDays();
      }
      let submitted = originalTargets.size ? draft : null;
      const lockChange = () => {
        for (const input of form.querySelectorAll('input,textarea')) input.disabled = true;
      };
      if (submitted) lockChange();
      wf.bindForm(dialog, async () => {
        const change = submitted || (notes ? {note:form.elements.note.value}
          : form.elements.duration.value === 'custom' ? {days:Number(form.elements.days.value)}
          : {months:Number(form.elements.duration.value)});
        if ('days' in change && (!Number.isSafeInteger(change.days) || change.days <= 0))
          throw new Error('请输入可精确表示的正整数天数');
        if (notes && Array.from(change.note).length > 2048) throw new Error('备注最多可填写 2048 个字符');
        let response;
        try {
          response = await editClients(action, targets, change);
        } catch (err) {
          submitted = change;
          lockChange();
          throw new Error(`${err.message}。可再次提交原请求以核对结果。`);
        }
        const byTarget = new Map(asList(response).map(r => [targetKey(r), r]));
        const failed = [];
        const results = entries.map(row => {
          const key = targetKey(row.target);
          const result = byTarget.get(key) || {...row.target, ok:false, message:'未收到此客户端的结果，请重新读取并核对'};
          if (result.ok) {
            if (notes) row.client.note = result.note;
            else row.client.expiryTime = result.expiryTime;
            row.client.updatedAt = result.updatedAt;
            selected.delete(key);
          } else failed.push(row);
          return result;
        });
        dialog.close();
        draw();
        showResults(`${title}结果`, results);
        if (!failed.length) return;
        const resultDialog = document.querySelector('dialog.wf-dialog[open]');
        const retry = document.createElement('button');
        retry.type = 'button';
        retry.className = 'wf-button';
        retry.textContent = `核对并重试失败项（${failed.length}）`;
        resultDialog.querySelector('.wf-dialog-foot').prepend(retry);
        retry.onclick = async () => {
          retry.disabled = true;
          try {
            // Only an explicit conflict proves that we can adopt a new base.
            const conflicts = failed.filter(r => byTarget.get(targetKey(r.target))?.conflict === true);
            const originals = new Map(targets.map(t => [targetKey(t), t]));
            const fresh = new Map();
            const panels = new Set(conflicts.map(r => r.panel));
            for (const panel of panels) {
              const inventory = await panelClients(ctx, wf, panel);
              for (const row of inventory.rows) fresh.set(targetKey(row.target), row);
            }
            if (!resultDialog.open || !root.isConnected) return;
            const keepOriginal = new Map();
            const remaining = failed.filter(r => {
              const key = targetKey(r.target);
              if (byTarget.get(key)?.conflict !== true) {
                keepOriginal.set(key, originals.get(key));
                return true;
              }
              if (!fresh.has(key)) return false;
              Object.assign(r, fresh.get(key));
              return true;
            });
            if (!remaining.length) throw new Error('所选客户端已移除或标识已变化，请刷新列表');
            if (remaining.length < failed.length) ctx.toast('部分客户端已移除或标识已变化');
            draw();
            editDialog(action, remaining, change, keepOriginal);
          } catch (err) { ctx.toast(err.message,'error'); }
          finally { retry.disabled = false; }
        };
      });
    }
    async function remove(targets) {
      if(ctx.state.demo){
        return targets.map(t=>{
          const panel=ctx.state.panels.find(p=>p.id===t.panelId);
          const data=wf.demoData(panel);
          for(const inbound of data.inbounds){inbound.settings.clients=inbound.settings.clients.filter(c=>c.id!==t.clientId);}
          panel.clients=data.inbounds.reduce((n,i)=>n+i.settings.clients.length,0);
          return {...t,name:panel.name,ok:true,message:'已删除演示客户端'};
        });
      }
      const result=await ctx.api('/api/clients/delete',{method:'POST',body:{targets}});
      return result.results;
    }
    function showResults(title,results) {
      wf.operationResults(ctx,title,results.map(r=>({...r,name:`${r.email} · ${r.name||panelName(r)}`})));
    }
    function confirmDelete(items) {
      const entries=[...new Map(items.map(row=>[targetKey(row.target),row])).values()];
      if(!entries.length)return;
      const scope=ctx.state.demo?'将从演示工作区删除所选客户端。':'将从远端 3x-ui 删除所选客户端及其全部入站绑定，并清理关联线路。';
      const dialog=wf.modal(ctx,'删除客户端','users',`<form id="wf-form" class="wf-form"><p>${scope}</p><ul class="wf-delete-targets">${entries.map(r=>`<li><strong>${esc(r.target.email)}</strong><small>${esc(panelName(r.target))} · ${esc(r.bindings.map(label).join('、')||'未绑定入站')}</small></li>`).join('')}</ul><div class="wf-inline-error" role="alert"></div></form>`,wf.submitFooter(`确认删除 ${entries.length} 个客户端`));
      dialog.querySelector('[data-wf-submit]').classList.add('wf-danger');
      wf.bindForm(dialog,async()=>{
        const results=await remove(entries.map(row=>row.target));
        dialog.close();showResults('客户端删除结果',results);await ctx.refresh();
      });
    }
    table.onchange=e=>{
      const tr=e.target.closest('[data-row]');if(!tr)return;
      const row=rows[Number(tr.dataset.row)];
      if(e.target.matches('[data-select-client]')){select(row,e.target.checked);draw();}
      if(e.target.matches('[data-share-inbound]'))chosen.set(targetKey(row.target),e.target.value);
    };
    table.onclick=async e=>{
      const btn=e.target.closest('button'),tr=btn?.closest('[data-row]');if(!tr)return;
      const row=rows[Number(tr.dataset.row)], item=selectedLink(row);
      if(btn.matches('[data-row-status]'))return changeStatus(row, Number(tr.dataset.row));
      if(btn.matches('[data-row-note]'))return editDialog('notes',[row]);
      if(btn.matches('[data-row-extend]'))return editDialog('extend',[row]);
      if(btn.matches('[data-row-quota]'))return trafficDialog('quota',[row]);
      if(btn.matches('[data-row-reset]'))return trafficDialog('reset-traffic',[row]);
      if(btn.matches('[data-remove-client]'))return confirmDelete([row]);
      if(btn.matches('[data-row-expiry]'))return wf.open('client.expiry',ctx,{panelId:row.panel.id,client:row.client,inbound:row.bindings[0]});
      if(!item)return;
      if(btn.matches('[data-show-qr]'))return window.Lines.share(item,ctx);
      if(btn.matches('[data-copy-link]'))return copyLinks([row]);
      if(btn.matches('[data-row-export]'))return exportNodes([row]);
    };
    function drawPending() {
      const slot=root.querySelector('[data-pending]');
      slot.innerHTML=pending.length?`<section class="wf-pending"><h2>待完成的删除</h2>${pending.map((t,i)=>`<div><span><strong>${esc(t.email)}</strong> · ${esc(panelName(t))}<small>${esc(t.error||'需要再次清理关联配置')}</small></span><button type="button" class="wf-button" data-retry="${i}">重试清理</button></div>`).join('')}</section>`:'';
      slot.onclick=async e=>{
        const btn=e.target.closest('[data-retry]');if(!btn||btn.disabled)return;
        const {panelId,clientId,email,uuid}=pending[Number(btn.dataset.retry)];btn.disabled=true;
        try{const results=await remove([{panelId,clientId,email,uuid}]);showResults('清理结果',results);await ctx.refresh();}
        catch(err){ctx.toast(err.message,'error');btn.disabled=false;}
      };
    }
    batch.onclick=()=>confirmDelete([...selected.values()]);
    root.querySelector('[data-copy-selected]').onclick=()=>copyLinks([...selected.values()]);
    root.querySelector('[data-export-selected]').onclick=()=>exportNodes([...selected.values()]);
    root.querySelector('[data-notes-selected]').onclick=()=>editDialog('notes',[...selected.values()]);
    root.querySelector('[data-extend-selected]').onclick=()=>editDialog('extend',[...selected.values()]);
    root.querySelector('[data-quota-selected]').onclick=()=>trafficDialog('quota',[...selected.values()]);
    root.querySelector('[data-reset-selected]').onclick=()=>trafficDialog('reset-traffic',[...selected.values()]);
    root.querySelector('[type=search]').oninput=()=>{page=0;draw();};
    root.querySelector('[data-filter]').onchange=()=>{page=0;draw();};
    const pendingRequest=ctx.state.demo?Promise.resolve({pending:[]}):ctx.api('/api/clients/deletions');
    const panels=ctx.state.panels.slice();
    const [inventory,cleanups]=await Promise.allSettled([
      (async()=>{
        for(let i=0;i<panels.length;i+=6){
          const results=await Promise.allSettled(panels.slice(i,i+6).map(p=>panelClients(ctx,wf,p)));
          if(!root.isConnected)return;
          results.forEach((r,offset)=>{
            const panel=panels[i+offset];
            if(r.status==='rejected'){warnings.push(`${panel.name} · ${r.reason.message}`);return;}
            rows.push(...r.value.rows);
            for(const warning of r.value.warnings)warnings.push(`${panel.name} · ${warning}`);
          });
        }
      })(),pendingRequest
    ]);
    if(!root.isConnected)return;
    if(inventory.status==='rejected')warnings.push(inventory.reason.message);
    if(cleanups.status==='fulfilled')pending.push(...asList(cleanups.value.pending));else warnings.push(cleanups.reason.message);
    root.querySelector('[data-warnings]').innerHTML=warnings.map(w=>`<p class="wf-alert">${esc(w)}</p>`).join('');
    drawPending();draw();
  }
  window.Clients={render};
})();
