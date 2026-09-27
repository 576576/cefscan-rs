// cefscanw 前端。刻意不引入任何打包工具：这里只有浏览器原生 API +
// Tauri 注入的 window.__TAURI__ 全局对象（tauri.conf.json 里 withGlobalTauri = true），
// 所以整个 GUI 用 `cargo build --release` 就能产出，不需要 Node/npm。

const MAX_ROWS = 500;

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
    parts.push(
      `<tr data-path="${escapeHtml(row.path)}">
        <td><span class="tag" style="background:${color}" title="${escapeHtml(detail)}">${escapeHtml(row.kind)}</span></td>
        <td class="num">${humanSize(row.size)}</td>
        <td class="num">${row.running ? '<span class="dot"></span>运行中' : ''}</td>
        <td class="path" title="${escapeHtml(row.path)}">${escapeHtml(row.path)}</td>
      </tr>`
    );
  }
  if (rows.length > visible.length) {
    parts.push(
      `<tr><td colspan="4" class="more">仅显示前 ${visible.length} 条，共 ${rows.length} 条</td></tr>`
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
    const row = event.target.closest('tr');
    const path = row && row.getAttribute('data-path');
    if (path) {
      invoke('reveal', { path }).catch((error) => {
        status.textContent = `无法定位：${String(error)}`;
      });
    }
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
