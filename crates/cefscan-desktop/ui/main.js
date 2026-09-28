// cefscanw 前端。刻意不引入任何打包工具：这里只有浏览器原生 API +
// Tauri 注入的 window.__TAURI__ 全局对象（tauri.conf.json 里 withGlobalTauri = true），
// 所以整个 GUI 用 `cargo build --release` 就能产出，不需要 Node/npm。

const MAX_ROWS = 500;

/** 折叠路径时前面保留的目录层数（盘符 / UNC 根不计入）。 */
const PATH_HEAD_SEGMENTS = 3;

const KIND_COLORS = {
  electron: '#7dc4e4',
  edge: '#5aa9e6',
  chrome: '#e06c75',
  nwjs: '#c678dd',
  cefsharp: '#e5c07b',
  mini_electron: '#98c379',
  mini_blink: '#56b6c2',
  cef: '#d19a66',
  unknown: '#7f848e',
};

const body = document.getElementById('results-body');
const empty = document.getElementById('empty');
const status = document.getElementById('status');
const button = document.getElementById('scan-button');
const rootInput = document.getElementById('root-input');
const backendSelect = document.getElementById('backend-select');
const summary = document.getElementById('summary');

// 由 tauri.conf.json 的 withGlobalTauri = true 注入；在普通浏览器里打开时为 undefined。
const { invoke, Channel } = window.__TAURI__ ? window.__TAURI__.core : {};

let rows = [];
let sortKey = 'size';
let sortAscending = false;
let renderQueued = false;
let scanning = false;

/** 已展开路径的行。用 Set 而不是给 <tr> 挂 class，是因为流式扫描会整表重绘。 */
const expandedPaths = new Set();

/** 1024 进制的人类可读体积，与 CLI 的 human_size 保持一致。 */
function humanSize(bytes) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return index === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[index]}`;
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/**
 * 折叠路径：前面只留「根 + 3 层目录」，中间省略号，后面只留文件名。
 *
 *   C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe
 *   → C:\Users\16695\AppData\…\WorkBuddy.exe
 *
 * 按分隔符切段而不是按字符数切，这样尾部的文件名一定完整。
 * 盘符（`C:`）和 UNC 的空段都算"根"，不占目录层数。
 */
function foldPath(path) {
  const separator = path.includes('\\') ? '\\' : '/';
  const segments = path.split(separator);
  const head = segments.slice(0, PATH_HEAD_SEGMENTS + 1).join(separator);
  const tail = segments[segments.length - 1];
  const folded = `${head}${separator}…${separator}${tail}`;
  // 短路径折完反而更长，那就别折。
  return folded.length < path.length ? folded : path;
}

/** 路径列的 HTML：折叠时只有一行摘要，展开时是完整路径 + 定位按钮。 */
function pathCellHtml(path, expanded) {
  if (!expanded) {
    return `<span class="path-folded" title="${escapeHtml(path)}">${escapeHtml(foldPath(path))}</span>`;
  }
  return (
    `<span class="path-full">${escapeHtml(path)}</span>` +
    `<button type="button" class="reveal" data-reveal="${escapeHtml(path)}">在资源管理器中显示</button>`
  );
}

/** 只重画一行的路径单元格——展开/收起不该触发整表重绘。 */
function paintPathCell(row) {
  const cell = row.querySelector('td.path');
  if (!cell) return;
  const path = row.dataset.path;
  const expanded = expandedPaths.has(path);
  row.classList.toggle('expanded', expanded);
  cell.innerHTML = pathCellHtml(path, expanded);
}

function sortRows() {
  rows.sort((left, right) => {
    let order;
    switch (sortKey) {
      case 'size':
        order = left.size - right.size;
        break;
      case 'running':
        order = Number(left.running) - Number(right.running);
        break;
      case 'kind':
        order = left.kind.localeCompare(right.kind);
        break;
      case 'name':
        order = left.name.localeCompare(right.name);
        break;
      default:
        order = left.path.localeCompare(right.path);
    }
    // 主键相同时用路径兜底，保证排序结果稳定、不会因为流式插入而抖动。
    if (order === 0) order = left.path.localeCompare(right.path);
    return sortAscending ? order : -order;
  });
}

function render() {
  renderQueued = false;
  empty.hidden = rows.length > 0;

  // 全盘扫描可能有上千条结果，只渲染前 MAX_ROWS 条，避免 DOM 过大拖慢 WebView。
  const visible = rows.slice(0, MAX_ROWS);
  const parts = [];
  for (const row of visible) {
    const color = KIND_COLORS[row.kind] || KIND_COLORS.unknown;
    const detail = row.evidence ? `${row.kind} · ${row.evidence}` : row.kind;
    const expanded = expandedPaths.has(row.path);
    const icon = row.icon ? `<img src="${row.icon}" alt="" width="18" height="18" />` : '';
    parts.push(
      `<tr data-path="${escapeHtml(row.path)}"${expanded ? ' class="expanded"' : ''}>
        <td class="icon">${icon}</td>
        <td class="name" title="${escapeHtml(row.name)}">${escapeHtml(row.name)}</td>
        <td><span class="tag" style="background:${color}" title="${escapeHtml(detail)}">${escapeHtml(row.kind)}</span></td>
        <td class="num">${humanSize(row.size)}</td>
        <td class="num">${row.running ? '<span class="dot"></span>运行中' : ''}</td>
        <td class="path">${pathCellHtml(row.path, expanded)}</td>
      </tr>`
    );
  }
  if (rows.length > visible.length) {
    parts.push(
      `<tr><td colspan="6" class="more">仅显示前 ${visible.length} 条，共 ${rows.length} 条</td></tr>`
    );
  }
  body.innerHTML = parts.join('');
}

/** 流式扫描时每条结果都会触发重绘，用 rAF 合并成每帧最多一次。 */
function scheduleRender() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(render);
}

function setScanning(value) {
  scanning = value;
  button.disabled = value;
  button.textContent = value ? '扫描中…' : '开始扫描';
}

async function runScan() {
  if (scanning) return;
  setScanning(true);
  rows = [];
  expandedPaths.clear();
  render();
  summary.hidden = true;
  status.textContent = '扫描中…';

  const channel = new Channel();
  channel.onmessage = (event) => {
    if (event.type === 'item') {
      // Item 事件的载荷就是 AppRow 本身，type 字段由 serde 打标签注入。
      const { type, ...row } = event;
      rows.push(row);
      sortRows();
      scheduleRender();
      status.textContent = `已找到 ${rows.length} 个…`;
      return;
    }
    if (event.type === 'done') {
      summary.hidden = false;
      document.getElementById('sum-apps').textContent = String(event.apps);
      document.getElementById('sum-total').textContent = humanSize(event.totalBytes);
      document.getElementById('sum-sum').textContent = humanSize(event.sumBytes);
      document.getElementById('sum-backend').textContent = event.backend;
      document.getElementById('sum-elapsed').textContent = `${event.elapsedMs} ms`;
      status.textContent = rows.length === 0 ? '没有找到应用' : `完成，共 ${rows.length} 个`;
      setScanning(false);
      return;
    }
    if (event.type === 'error') {
      status.textContent = `失败：${event.message}`;
      setScanning(false);
    }
  };

  const root = rootInput.value.trim();
  try {
    await invoke('scan_apps', {
      channel,
      request: { roots: root ? [root] : [], backend: backendSelect.value, threads: 0 },
    });
  } catch (error) {
    status.textContent = `失败：${String(error)}`;
    setScanning(false);
  }
}

function bootstrap() {
  if (!window.__TAURI__ || !window.__TAURI__.core) {
    status.textContent = '未检测到 Tauri 运行时';
    button.disabled = true;
    return;
  }

  button.addEventListener('click', () => void runScan());
  rootInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') void runScan();
  });
  backendSelect.addEventListener('change', () => {
    rootInput.focus();
  });

  body.addEventListener('click', (event) => {
    // 展开后才出现的"在资源管理器中显示"按钮：拦下来，别让它顺带收起行。
    const reveal = event.target.closest('[data-reveal]');
    if (reveal) {
      event.stopPropagation();
      invoke('reveal', { path: reveal.dataset.reveal }).catch((error) => {
        status.textContent = `无法定位：${String(error)}`;
      });
      return;
    }

    // 点整行 = 展开/收起该行的完整路径。
    const row = event.target.closest('tr');
    if (!row || !row.dataset.path) return;
    const path = row.dataset.path;
    if (expandedPaths.has(path)) {
      expandedPaths.delete(path);
    } else {
      expandedPaths.add(path);
    }
    paintPathCell(row);
  });

  for (const header of document.querySelectorAll('th[data-sort]')) {
    header.addEventListener('click', () => {
      const key = header.dataset.sort;
      if (key === sortKey) {
        sortAscending = !sortAscending;
      } else {
        sortKey = key;
        sortAscending = false;
      }
      sortRows();
      render();
    });
  }

  render();
}

bootstrap();
