const $ = id => document.getElementById(id);
const esc = s => { const d = document.createElement('div'); d.textContent = s; return d.innerHTML; };
const eAttr = s => s.replace(/'/g, "\\'");
const api = async (path, method = 'GET', body) => {
  const o = { method, headers: {} };
  if (body) { o.headers['Content-Type'] = 'application/json'; o.body = JSON.stringify(body); }
  const r = await fetch(path, o); if (!r.ok) throw new Error(await r.text()); return r;
};
const setupDropzone = (dzId, inpId, onFiles) => {
  const dz = $(dzId), inp = $(inpId);
  dz.onclick = () => inp.click();
  dz.ondragover = e => { e.preventDefault(); dz.classList.add('dragover'); };
  dz.ondragleave = () => dz.classList.remove('dragover');
  dz.ondrop = e => { e.preventDefault(); dz.classList.remove('dragover'); onFiles(e.dataTransfer.files); };
  inp.onchange = e => onFiles(e.target.files);
};

let currentDraft = '', selectedText = '', currentRawHTML = '', currentHTML = '';
let annotations = [], debugLog = [], currentMode = 'preview';
let photoData = [];
let statsUsd = true;

// === UI ===
function showToast(msg, type = 'info') {
  const t = document.createElement('div'); t.className = 'toast ' + type; t.textContent = msg;
  t.style.cursor = 'pointer'; t.onclick = () => { t.remove(); };
  $('toast-container').appendChild(t);
  const ms = type === 'error' ? 8000 : type === 'warning' ? 6000 : 4000;
  setTimeout(() => { t.classList.add('fadeout'); setTimeout(() => t.remove(), 300); }, ms);
}
let _confirmRes;
function showConfirm(msg) { return new Promise(r => { $('confirm-msg').textContent = msg; $('confirm-modal').classList.add('visible'); _confirmRes = r; }); }
function resolveConfirm(r) { $('confirm-modal').classList.remove('visible'); if (_confirmRes) { _confirmRes(r); _confirmRes = null; } }
function showProgress(m) { $('progress-text').textContent = m; }
function showLoading(s) { $('loading').classList.toggle('visible', s); $('generate-btn').disabled = s; }
async function cancelTask() { try { await fetch('/api/cancel', { method: 'POST' }); } catch {} showProgress('正在中断...'); }
function updateWordCount() { $('word-count').textContent = currentDraft.replace(/\s/g, '').length + ' 字'; }
function download(content, name, type) {
  const b = new Blob([content], { type: type + ';charset=utf-8' }); const u = URL.createObjectURL(b);
  const a = document.createElement('a'); a.href = u; a.download = name; a.click(); URL.revokeObjectURL(u);
}

// === SSE ===
async function streamSSE(url, body, onP, onD, onE) {
  const r = await fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const rd = r.body.getReader(); const d = new TextDecoder(); let b = '';
  for (;;) {
    const { done, value } = await rd.read(); if (done) break;
    b += d.decode(value, { stream: true });
    const p = b.split(/\r?\n\r?\n/); b = p.pop();
    for (const part of p) {
      let et = 'msg', data = '';
      for (const l of part.split(/\r?\n/)) {
        if (l.startsWith('event:')) et = l.slice(6).trim();
        else if (l.startsWith('data:')) data += l.slice(5).trim();
      }
      if (et === 'progress') onP(JSON.parse(data).msg);
      else if (et === 'done') onD(JSON.parse(data));
      else if (et === 'error') onE(JSON.parse(data).msg);
      else if (et === 'cancelled') onE('已中断');
    }
  }
}
async function runSSE(url, body, onDone) {
  showLoading(true); showProgress('准备中...');
  try { await streamSSE(url, body, showProgress, d => { onDone(d); updateStats(); }, m => showToast(m, 'error')); }
  catch (e) { showToast(e.message, 'error'); }
  showLoading(false);
}

// === Materials ===
setupDropzone('dropzone', 'file-input', async files => {
  for (const f of files) { const fd = new FormData(); fd.append('file', f); try { await fetch('/api/materials', { method: 'POST', body: fd }); } catch { showToast('上传失败: ' + f.name, 'error'); } }
  loadMaterials();
});
async function loadMaterials() {
  const list = await (await fetch('/api/materials')).json();
  $('materials').innerHTML = list.map(m => {
    const sz = m.size > 1024 ? (m.size / 1024).toFixed(1) + ' KB' : m.size + ' B';
    return `<div class="mat-item"><span class="name">${esc(m.name)} (${sz})</span><span class="remove" onclick="removeMaterial('${eAttr(m.name)}')">✕</span></div>`;
  }).join('');
}
async function removeMaterial(name) { await fetch('/api/materials/' + encodeURIComponent(name), { method: 'POST' }); loadMaterials(); }

// === Generate ===
async function generate() {
  const topic = $('topic').value.trim();
  if (!topic) return showToast('请输入选题', 'warning');
  if (currentDraft && !await showConfirm('当前已有文章内容，生成新文章将覆盖。是否继续？')) return;
  await runSSE('/api/write', { topic, article_type: $('article-type').value }, d => {
    currentDraft = d.content; renderArticle(d.content); logApiCall('生成', d);
  });
}

function renderArticle(md) {
  $('placeholder').style.display = 'none';
  const a = $('article'), e = $('article-edit');
  $('article-toolbar').style.display = 'flex';
  currentMode = 'preview';
  a.innerHTML = marked.parse(md); a.style.display = 'block'; e.style.display = 'none'; e.oninput = null;
  $('mode-preview').classList.add('active'); $('mode-edit').classList.remove('active');
  e.value = md;
  highlightAnnotations();
  updateWordCount();
}

async function setMode(mode) {
  if (mode === 'edit' && annotations.length > 0) {
    const submit = await showConfirm(`有 ${annotations.length} 条未提交的批注，是否先提交？`);
    if (submit) { await submitAllAnnotations(); if (annotations.length > 0) return; }
    else {
      const discard = await showConfirm('是否丢弃批注进入编辑模式？');
      if (!discard) return;
      annotations = []; renderAnnotations();
    }
  }
  const a = $('article'), e = $('article-edit');
  if (mode === 'preview' && currentMode === 'edit') currentDraft = e.value;
  if (mode === 'edit' && currentMode === 'preview') { currentDraft = e.value || currentDraft; e.value = currentDraft; }
  currentMode = mode;
  if (mode === 'edit') {
    e.value = currentDraft; a.style.display = 'none'; e.style.display = 'block';
    $('mode-edit').classList.add('active'); $('mode-preview').classList.remove('active');
    e.oninput = () => { currentDraft = e.value; updateWordCount(); };
  } else {
    a.innerHTML = marked.parse(currentDraft); a.style.display = 'block'; e.style.display = 'none'; e.oninput = null;
    $('mode-preview').classList.add('active'); $('mode-edit').classList.remove('active');
    highlightAnnotations();
  }
  updateWordCount();
}

// === Import / Export ===
$('import-input').onchange = e => importFile(e.target.files[0]);
async function importFile(file) {
  if (!file) return;
  if (currentDraft && !await showConfirm('当前已有文章内容，导入将覆盖。是否继续？')) return;
  showLoading(true);
  try { const fd = new FormData(); fd.append('file', file); const r = await fetch('/api/import', { method: 'POST', body: fd }); if (!r.ok) throw new Error(await r.text());
    const d = await r.json(); currentDraft = d.content; renderArticle(d.content); }
  catch (e) { showToast('导入失败: ' + e.message, 'error'); }
  showLoading(false);
}
function exportArticle() { if (!currentDraft) return showToast('暂无文章', 'warning'); download(currentDraft, 'article.md', 'text/markdown'); }
function copyArticle() { if (!currentDraft) return showToast('暂无文章', 'warning'); navigator.clipboard.writeText(currentDraft).then(() => showToast('已复制', 'success')).catch(() => showToast('复制失败', 'error')); }

// === Annotations ===
$('article').addEventListener('mouseup', () => {
  setTimeout(() => {
    const sel = window.getSelection(); const t = sel.toString().trim();
    if (t && $('article').contains(sel.anchorNode)) { selectedText = t; $('sel-indicator').textContent = '已选: ' + (t.length > 50 ? t.slice(0, 50) + '...' : t); }
    else { selectedText = ''; $('sel-indicator').textContent = '未选中文字'; }
  }, 10);
});
function addAnnotationFromSelection() {
  if (!selectedText) return showToast('请先选中文字', 'warning');
  annotations.push({ selected_text: selectedText, comment: '' });
  selectedText = ''; window.getSelection().removeAllRanges();
  $('sel-indicator').textContent = '未选中文字'; renderAnnotations(); highlightAnnotations();
}
function highlightAnnotations() {
  const a = $('article'); if (a.style.display === 'none') return;
  a.querySelectorAll('.annot-mark').forEach(el => { el.parentNode.replaceChild(document.createTextNode(el.textContent), el); el.parentNode.normalize(); });
  annotations.forEach(ann => { if (ann.selected_text) highlightText(a, ann.selected_text); });
}
function highlightText(root, text) {
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); const ns = [];
  let n; while (n = w.nextNode()) if (n.textContent.includes(text) && n.parentElement.tagName !== 'SCRIPT') ns.push(n);
  for (const tn of ns.slice(0, 1)) {
    const t = tn.textContent, i = t.indexOf(text); if (i === -1) continue;
    const s = document.createElement('span'); s.className = 'annot-mark'; s.textContent = text;
    const p = tn.parentNode;
    if (i > 0) p.insertBefore(document.createTextNode(t.slice(0, i)), tn);
    p.insertBefore(s, tn);
    if (i + text.length < t.length) p.insertBefore(document.createTextNode(t.slice(i + text.length)), tn);
    p.removeChild(tn);
  }
}
function renderAnnotations() {
  const c = $('annot-list'); c.innerHTML = '';
  $('submit-annot-btn').style.display = annotations.length ? 'block' : 'none';
  annotations.forEach((a, i) => {
    const q = a.selected_text.length > 120 ? a.selected_text.slice(0, 120) + '...' : a.selected_text;
    c.innerHTML += `<div class="annot-item"><div class="annot-header"><span style="font-size:12px;color:var(--text-light)">批注 ${i + 1}</span><button class="annot-del" onclick="removeAnnotation(${i})">✕</button></div><div class="annot-quote">${esc(q)}</div><textarea placeholder="输入修改意见" oninput="annotations[${i}].comment = this.value">${esc(a.comment)}</textarea></div>`;
  });
}
function removeAnnotation(i) { annotations.splice(i, 1); renderAnnotations(); highlightAnnotations(); }
async function submitAllAnnotations() {
  if (!annotations.length) return showToast('请先添加批注', 'warning');
  const empty = annotations.filter(a => !a.comment.trim());
  if (empty.length && !await showConfirm(`有 ${empty.length} 条批注未填写意见，是否跳过？`)) return;
  const valid = annotations.filter(a => a.comment.trim());
  await runSSE('/api/revise', { draft: currentDraft, annotations: valid }, d => {
    currentDraft = d.content; renderArticle(d.content); logApiCall('修订', d);
    annotations = []; renderAnnotations();
  });
}

// === Stats ===
$('stats').addEventListener('click', e => { if (!e.target.closest('#settings-btn')) { statsUsd = !statsUsd; updateStats(); } });
async function updateStats() {
  const d = await (await fetch('/api/stats')).json();
  $('stat-calls').textContent = d.call_count;
  $('stat-tokens').textContent = d.total_tokens;
  $('stat-cost-symbol').textContent = statsUsd ? '$' : '¥';
  $('stat-cost').textContent = (statsUsd ? d.total_cost : d.total_cost_cny).toFixed(4);
}

// === Settings ===
async function openSettings() {
  const c = await (await fetch('/api/config')).json();
  $('cfg-endpoint').value = c.endpoint || ''; $('cfg-model').value = c.model || '';
  $('cfg-context-length').value = c.context_length || ''; $('cfg-max-tokens').value = c.max_tokens || '';
  $('cfg-temperature').value = c.temperature || '';
  $('cfg-thinking-mode').value = c.thinking_mode ? 'true' : 'false';
  $('cfg-input-price').value = c.pricing?.input_per_1k ?? ''; $('cfg-output-price').value = c.pricing?.output_per_1k ?? '';
  $('cfg-api-key').value = ''; $('cfg-key-current').textContent = '当前: ' + c.api_key_masked;
  $('cfg-vision-endpoint').value = c.vision_endpoint || '';
  $('cfg-vision-model').value = c.vision_model || '';
  $('cfg-vision-max-tokens').value = c.vision_max_tokens || '';
  $('cfg-vision-desc-chars').value = c.vision_desc_max_chars || '';
  $('cfg-vision-key').value = '';
  $('cfg-wx-app-id').value = c.wx_app_id || '';
  $('cfg-wx-app-secret').value = '';
  $('cfg-wx-app-secret').placeholder = c.wx_configured ? '已配置（留空不修改）' : '未配置';
  $('cfg-token-budget').value = c.token_budget || '';
  $('settings-modal').classList.add('visible');
}
function cfgForm() {
  const v = id => $(id).value.trim(); const n = id => parseFloat($(id).value) || null;
  return {
    endpoint: v('cfg-endpoint') || null, model: v('cfg-model') || null,
    context_length: n('cfg-context-length'), max_tokens: n('cfg-max-tokens'),
    temperature: n('cfg-temperature'), thinking_mode: $('cfg-thinking-mode').value === 'true',
    api_key: v('cfg-api-key') || null,
    pricing: { input_per_1k: n('cfg-input-price'), output_per_1k: n('cfg-output-price') },
    vision_endpoint: v('cfg-vision-endpoint') || null,
    vision_api_key: v('cfg-vision-key') || null,
    vision_model: v('cfg-vision-model') || null,
    vision_max_tokens: n('cfg-vision-max-tokens'),
    vision_desc_max_chars: n('cfg-vision-desc-chars'),
    wx_app_id: v('cfg-wx-app-id') || null,
    wx_app_secret: v('cfg-wx-app-secret') || null,
    token_budget: n('cfg-token-budget'),
  };
}
// Ask the server whether the form differs from the stored config
async function closeSettings() {
  try {
    const { changed } = await (await fetch('/api/settings/check', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(cfgForm()) })).json();
    if (changed && !await showConfirm('设置有未保存的修改，确定要放弃修改并关闭吗？')) return;
  } catch {}
  $('settings-modal').classList.remove('visible');
}
async function saveConfig() {
  try {
    await api('/api/config', 'POST', cfgForm());
    $('settings-modal').classList.remove('visible'); showToast('配置已保存', 'success');
  } catch (e) { showToast('保存失败: ' + e.message, 'error'); }
}

// === Debug ===
function toggleDebug() { const p = $('debug-panel'); p.classList.toggle('visible'); $('debug-toggle').textContent = p.classList.contains('visible') ? 'Console ▲' : 'Console ▼'; }
function clearDebug() { $('debug-content').innerHTML = ''; debugLog = []; }
function saveDebug() {
  if (!debugLog.length) return showToast('日志为空', 'warning');
  let t = '';
  for (const e of debugLog) {
    t += `[${e.ts}] ${e.type}\n`;
    for (const m of e.messages) t += `[${m.role.toUpperCase()}] ${m.content}\n`;
    if (e.html) t += `[HTML] ${e.html}\n`;
    if (e.usage) t += `tokens: in=${e.usage.prompt_tokens} out=${e.usage.completion_tokens} total=${e.usage.total_tokens}\n`;
    t += '\n';
  }
  download(t, 'api_log.txt', 'text/plain'); showToast('日志已保存', 'success');
}
function logApiCall(type, data) {
  const e = { ts: new Date().toLocaleTimeString(), type, messages: data.debug_messages || [], usage: data.usage, html: data.html };
  debugLog.push(e);
  const c = $('debug-content'); const div = document.createElement('div'); div.className = 'debug-entry';
  let h = `<div class="ts">[${e.ts}] ${type}</div>`;
  for (const m of e.messages) {
    const cls = m.role === 'system' ? 'sys' : m.role === 'user' ? 'usr' : 'ast';
    h += `<div><span class="tag ${cls}">${m.role.toUpperCase()}</span></div><pre>${esc(m.content)}</pre>`;
  }
  if (e.html) h += `<div><span class="tag ast">HTML</span></div><pre>${esc(e.html)}</pre>`;
  if (e.usage) h += `<div class="meta">tokens: in=${e.usage.prompt_tokens} out=${e.usage.completion_tokens} total=${e.usage.total_tokens}</div>`;
  div.innerHTML = h; c.appendChild(div); c.scrollTop = c.scrollHeight;
}

// === Format ===
function enterFormat() {
  if (!currentDraft) return showToast('请先生成文章', 'warning');
  $('article-area').style.display = 'none';
  $('annot-sidebar').style.display = 'none';
  $('format-view').style.display = 'flex';
  const b = $('format-toggle-btn');
  b.textContent = '返回编辑';
  b.onclick = exitFormat;
  loadFormatPhotos();
  loadTemplates();
}
function exitFormat() {
  $('format-view').style.display = 'none';
  $('article-area').style.display = 'block';
  $('annot-sidebar').style.display = 'flex';
  const b = $('format-toggle-btn');
  b.textContent = '进入排版';
  b.onclick = enterFormat;
}

async function loadTemplates() {
  const templates = [
    { id: 'clean_blue', name: '蓝白简洁风' },
    { id: 'warm_earth', name: '暖色大地风' },
    { id: 'tech_dark', name: '深蓝科技风' },
    { id: 'festival_red', name: '节日红色风' },
    { id: 'minimal_bw', name: '黑白极简风' },
    { id: 'fresh_green', name: '清新绿色风' },
  ];
  const sel = $('template-select');
  sel.innerHTML = '<option value="">AI 自动选择</option>';
  for (const t of templates) {
    sel.innerHTML += `<option value="${t.id}">${t.name}</option>`;
  }
}
setupDropzone('fmt-photo-dropzone', 'fmt-photo-input', async files => {
  for (const f of files) {
    if (!f.type.startsWith('image/')) continue;
    const fd = new FormData(); fd.append('file', f);
    const dim = await new Promise(res => {
      const im = new Image(); const url = URL.createObjectURL(f);
      im.onload = () => { res([im.naturalWidth, im.naturalHeight]); URL.revokeObjectURL(url); };
      im.onerror = () => { res([0, 0]); URL.revokeObjectURL(url); };
      im.src = url;
    });
    fd.append('width', dim[0]); fd.append('height', dim[1]);
    try { const r = await fetch('/api/photos', { method: 'POST', body: fd }); if (!r.ok) throw new Error(r.statusText); }
    catch (e) { showToast('照片上传失败: ' + f.name + ' (' + e.message + ')', 'error'); }
  }
  loadFormatPhotos();
});
async function loadFormatPhotos() {
  const list = await (await fetch('/api/photos')).json();
  photoData = list;
  $('fmt-photos').innerHTML = list.map((p, i) =>
    `<div class="fmt-photo-row" data-name="${eAttr(p.name)}" style="display:flex;gap:8px;align-items:flex-start;margin-bottom:8px">` +
    `<div class="photo-item" style="flex-shrink:0;cursor:pointer" onclick="openImageModal('${eAttr(p.name)}')">` +
    `<img src="/api/photo-img/${encodeURIComponent(p.name)}"><button class="photo-del" onclick="event.stopPropagation();removePhoto('${eAttr(p.name)}')">✕</button></div>` +
    `<div style="flex:1;display:flex;flex-direction:column;gap:4px">` +
    `<input type="text" class="photo-desc-input" value="${esc(p.description)}" placeholder="描述照片内容" onchange="updatePhotoDesc('${eAttr(p.name)}',this.value)" style="font-size:13px;padding:6px 8px;border:1px solid var(--border);border-radius:6px;outline:none">` +
    `<button class="secondary photo-recognize-btn" data-name="${eAttr(p.name)}" onclick="recognizePhoto('${eAttr(p.name)}',this)" style="font-size:12px;padding:4px 8px;width:auto">AI 识图</button>` +
    `</div></div>`
  ).join('');
}

function openImageModal(name) {
  const photos = document.querySelectorAll('#fmt-photos .photo-item img');
  for (const img of photos) {
    if (img.src && img.closest('.photo-item').onclick) {
      fetch('/api/photos').then(r => r.json()).then(list => {
        const p = list.find(x => x.name === name);
        if (p) {
          $('image-large').src = p.data_url;
          $('image-desc-large').textContent = p.description || '';
          $('image-modal').classList.add('visible');
        }
      });
      break;
    }
  }
}
function closeImageModal() { $('image-modal').classList.remove('visible'); }

async function recognizeAllPhotos(btn) {
  const list = await (await fetch('/api/photos')).json();
  if (!list.length) return showToast('暂无照片', 'warning');
  const pending = list.filter(p => !p.description);
  if (!pending.length) return showToast('所有照片已有描述', 'warning');

  const orig = btn.textContent;
  btn.disabled = true;
  btn.textContent = `识别中 0/${pending.length}`;

  let done = 0;
  await Promise.all(pending.map(async (p) => {
    const row = document.querySelector(`.fmt-photo-row[data-name="${p.name.replace(/"/g,'&quot;')}"]`);
    const recBtn = row?.querySelector('.photo-recognize-btn');
    const descInput = row?.querySelector('.photo-desc-input');
    if (recBtn) { recBtn.textContent = '识别中...'; recBtn.disabled = true; }
    try {
      const r = await fetch('/api/photos/' + encodeURIComponent(p.name) + '/recognize', { method: 'POST' });
      if (!r.ok) throw new Error(await r.text());
      const d = await r.json();
      logApiCall('识图', d);
      done++;
      btn.textContent = `识别中 ${done}/${pending.length}`;
      updateStats();
      // Update this photo's description input immediately
      if (descInput) descInput.value = d.description;
      if (recBtn) { recBtn.textContent = '✓ 已识别'; recBtn.style.color = '#27ae60'; recBtn.disabled = false; }
    } catch (e) {
      showToast('识图失败: ' + p.name, 'error');
      if (recBtn) { recBtn.textContent = 'AI 识图'; recBtn.disabled = false; }
    }
  }));

  // Update photoData with new descriptions from server
  try { photoData = await (await fetch('/api/photos')).json(); } catch {}
  btn.disabled = false;
  btn.textContent = orig;
  showToast(`识图完成 (${done}/${pending.length})`, 'success');
}
async function removePhoto(name) { await fetch('/api/photos/' + encodeURIComponent(name), { method: 'POST' }); loadFormatPhotos(); }
async function updatePhotoDesc(name, desc) { await api('/api/photos/' + encodeURIComponent(name) + '/description', 'POST', { description: desc }); }
async function recognizePhoto(name, btn) {
  btn.textContent = '识别中...'; btn.disabled = true;
  try {
    const r = await fetch('/api/photos/' + encodeURIComponent(name) + '/recognize', { method: 'POST' });
    if (!r.ok) throw new Error(await r.text());
    const d = await r.json();
    updateStats();
    logApiCall('识图', d);
    // Update description input in place
    const row = btn.closest('.fmt-photo-row');
    const descInput = row?.querySelector('.photo-desc-input');
    if (descInput) descInput.value = d.description;
    btn.textContent = '✓ 重新识图'; btn.disabled = false;
    showToast('识图完成', 'success');
  } catch (e) {
    showToast('识图失败: ' + e.message, 'error');
    btn.textContent = 'AI 识图'; btn.disabled = false;
  }
}
async function formatArticle() {
  $('format-preview').style.display = 'none'; $('format-actions').style.display = 'none';
  $('typo-controls').style.display = 'none';
  const template = $('template-select').value || null;
  await runSSE('/api/format', { draft: currentDraft, template }, d => {
    currentRawHTML = d.html || '';
    currentHTML = replacePlaceholdersForPreview(currentRawHTML);
    $('format-preview').innerHTML = currentHTML;
    $('format-preview').style.display = 'block'; $('format-actions').style.display = 'flex';
    $('typo-controls').style.display = 'flex'; $('r-pd').value = 20; applyTypo(); logApiCall('排版', d);
  });
}

function applyTypo() {
  const p = $('format-preview');
  const fs = $('r-fs').value;
  const lh = ($('r-lh').value / 10).toFixed(1);
  const ls = $('r-ls').value;
  const pd = $('r-pd').value;
  const pm = $('r-pm').value;
  p.style.setProperty('--fs', fs + 'px');
  p.style.setProperty('--lh', lh);
  p.style.setProperty('--ls', ls + 'px');
  p.style.setProperty('--pd', pd + 'px');
  p.style.setProperty('--pm', pm + 'px');
  $('v-fs').textContent = fs;
  $('v-lh').textContent = lh;
  $('v-ls').textContent = ls;
  $('v-pd').textContent = pd;
  $('v-pm').textContent = pm;
}

function resetTypo() {
  $('r-fs').value = 16; $('r-lh').value = 20; $('r-ls').value = 1; $('r-pd').value = 20; $('r-pm').value = 10;
  applyTypo();
}

function getTypoStyleTag() {
  const fs = $('r-fs').value;
  const lh = ($('r-lh').value / 10).toFixed(1);
  const ls = $('r-ls').value;
  const pd = $('r-pd').value;
  const pm = $('r-pm').value;
  return `<style>body{margin:0;padding:24px 0;background:#f5f5f5}div,p{max-width:375px!important;margin:0 auto!important}p,li{font-size:${fs}px!important;line-height:${lh}!important;letter-spacing:${ls}px!important;margin-bottom:${pm}px!important}ul,ol{font-size:${fs}px!important;line-height:${lh}!important;letter-spacing:${ls}px!important}div{padding-left:${pd}px!important;padding-right:${pd}px!important}</style>`;
}

// Replace {{photo:filename}} with server URL for local preview
function replacePlaceholdersForPreview(html) {
  let result = html;
  for (const p of photoData) {
    const placeholder = `{{photo:${p.name}}}`;
    const img = `<img src="/api/photo-img/${encodeURIComponent(p.name)}" style="width:100%;border-radius:8px;margin:10px 0" />`;
    result = result.split(placeholder).join(img);
  }
  return result;
}

// Replace {{photo:filename}} with base64 for clipboard copy (images travel with HTML)
function replacePlaceholdersForCopy(html) {
  let result = html;
  for (const p of photoData) {
    const placeholder = `{{photo:${p.name}}}`;
    const img = `<img src="${p.data_url}" style="width:100%;border-radius:8px;margin:10px 0" />`;
    result = result.split(placeholder).join(img);
  }
  return result;
}
function copyHTML() {
  if (!currentRawHTML) return showToast('暂无HTML', 'warning');
  const htmlForCopy = getTypoStyleTag() + replacePlaceholdersForCopy(currentRawHTML);
  const div = document.createElement('div');
  div.contentEditable = 'true'; div.style.position = 'fixed'; div.style.left = '-9999px';
  div.innerHTML = htmlForCopy;
  document.body.appendChild(div);
  const range = document.createRange(); range.selectNodeContents(div);
  const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(range);
  let ok = false;
  try { ok = document.execCommand('copy'); } catch {}
  sel.removeAllRanges(); document.body.removeChild(div);
  if (ok) { showToast('已复制，可在秀米/微信编辑器 Ctrl+V 粘贴', 'success'); return; }
  if (navigator.clipboard && navigator.clipboard.write) {
    const blob = new Blob([htmlForCopy], { type: 'text/html' });
    const item = new ClipboardItem({ 'text/html': blob, 'text/plain': new Blob([htmlForCopy], { type: 'text/plain' }) });
    navigator.clipboard.write([item]).then(
      () => showToast('已复制，可在秀米/微信编辑器 Ctrl+V 粘贴', 'success'),
      () => { fallbackCopy(htmlForCopy); }
    );
  } else { fallbackCopy(htmlForCopy); }
}
function fallbackCopy(html) {
  const ta = document.createElement('textarea');
  ta.value = html; document.body.appendChild(ta); ta.select();
  try { document.execCommand('copy'); showToast('已复制（纯文本模式）', 'success'); }
  catch { showToast('复制失败', 'error'); }
  document.body.removeChild(ta);
}

function downloadHTML() {
  if (!currentRawHTML) return showToast('暂无HTML', 'warning');
  const htmlForDownload = getTypoStyleTag() + currentRawHTML;
  showLoading(true); showProgress('正在打包...');
  fetch('/api/download-zip', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ html: htmlForDownload })
  }).then(r => r.blob()).then(blob => {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url; a.download = 'article.zip'; a.click();
    URL.revokeObjectURL(url);
    showToast('已下载 article.zip', 'success');
  }).catch(e => showToast('下载失败: ' + e.message, 'error'))
    .finally(() => showLoading(false));
}

async function publishDraft() {
  if (!currentRawHTML) return showToast('暂无HTML', 'warning');
  const htmlForPublish = getTypoStyleTag() + currentRawHTML;
  showLoading(true); showProgress('正在生成标题并推送草稿箱...');
  try {
    const r = await api('/api/publish', 'POST', { html: htmlForPublish, draft: currentDraft });
    const d = await r.json();
    showToast(`已推送：「${d.title}」`, 'success');
  } catch (e) { showToast('推送失败: ' + e.message, 'error'); }
  showLoading(false);
}

// === Session (R5) ===
async function saveSession() {
  try { await api('/api/session', 'POST', { topic: $('topic').value, article_type: $('article-type').value || null, draft: currentDraft, annotations }); showToast('会话已保存', 'success'); }
  catch (e) { showToast('保存失败: ' + e.message, 'error'); }
}
async function openSessionList() {
  const list = await (await fetch('/api/sessions')).json();
  const c = $('session-list'); c.innerHTML = list.length ? '' : '<p style="text-align:center;color:var(--text-light)">暂无会话</p>';
  for (const s of list) { const d = document.createElement('div'); d.className = 'session-entry'; d.onclick = () => loadSession(s.id); d.innerHTML = `<div class="sess-time">${esc(s.timestamp)}</div><div class="sess-topic">${esc(s.topic || '(无选题)')}</div>`; c.appendChild(d); }
  $('session-modal').classList.add('visible');
}
function closeSessionList() { $('session-modal').classList.remove('visible'); }
async function loadSession(id) {
  try {
    const d = await (await api('/api/session/' + id)).json();
    $('topic').value = d.topic || ''; $('article-type').value = d.article_type || '';
    currentDraft = d.draft || ''; annotations = d.annotations || [];
    renderArticle(currentDraft); renderAnnotations(); highlightAnnotations();
    loadMaterials(); loadFormatPhotos(); updateStats(); closeSessionList(); showToast('会话已加载', 'success');
  } catch (e) { showToast('加载失败', 'error'); }
}

// === Listeners ===
$('topic').addEventListener('keydown', e => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); generate(); } });
for (const [id, fn] of [['confirm-modal', () => resolveConfirm(false)], ['session-modal', closeSessionList]])
  $(id).addEventListener('click', e => { if (e.target === $(id)) fn(); });

// Init
loadMaterials(); updateStats();
