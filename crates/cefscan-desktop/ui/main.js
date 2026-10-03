// cefscanw 前端。刻意不引入任何打包工具：这里只有浏览器原生 API +
// Tauri 注入的 window.__TAURI__ 全局对象（tauri.conf.json 里 withGlobalTauri = true），
// 所以整个 GUI 用 `cargo build --release` 就能产出，不需要 Node/npm。

const MAX_ROWS = 500;

/** 折叠路径时前面保留的目录层数（盘符 / UNC 根不计入）。 */
const PATH_HEAD_SEGMENTS = 3;

/**
 * 后端显示文案。后端是自动挑的（有索引服务就用，没有就自己遍历），
 * 所以前缀固定是"自动"；真正有信息量的是括号里的**具体后端名**
 * （`cefscan` / `Everything`），而不是"遍历"/"索引"这类内部叫法。
 */
const BACKEND_PENDING = '自动（检测中…）';

function backendLabel(name) {
  return `自动（${name}）`;
}

/**
 * 经典模式的结果揭示节奏。
 *
 * 为什么要排队而不是收到就画：索引后端会在几百毫秒内一次吐出几十条结果，
 * 如果直接画出来，用户看到的是一整屏同时"啪"地出现——只有遍历后端那种
 * 天然一条条到达的节奏才自带"缓缓出现"的观感。排队 + 固定节奏让两种后端
 * 看起来一致。
 *
 * REVEAL_BUDGET_MS 是止损：500 条按 120ms 一条要一分钟，所以积压越多
 * 一次揭示得越多，总时长收敛在这个预算内（见 revealTick 的 step 计算）。
 */
const REVEAL_STEP_MS = 120;
const REVEAL_BUDGET_MS = 4000;

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
const backendDisplay = document.getElementById('backend-display');
const classicInput = document.getElementById('classic-input');
const summary = document.getElementById('summary');

// 由 tauri.conf.json 的 withGlobalTauri = true 注入；在普通浏览器里打开时为 undefined。
const { invoke, Channel } = window.__TAURI__ ? window.__TAURI__.core : {};

let rows = [];
let sortKey = 'size';
let sortAscending = false;
let renderQueued = false;
let scanning = false;

/** 经典模式：喜报背景 + 结果排队缓缓浮现。初值取自勾选框（HTML 里默认 checked）。 */
let classicMode = classicInput.checked;

/** 经典模式下已到达但还没揭示出去的结果。 */
let revealQueue = [];
let revealTimer = null;
/** 结果还在往外浮时先压住的汇总，等队列清空再显示。 */
let deferredDone = null;

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
    // 只有"还没画过"的行才带 .enter：整表重绘（排序、展开、新结果插入）时，
    // 已经在屏幕上的行不该重放一次入场动画。
    const entering = !row.painted;
    row.painted = true;
    const classes = [];
    if (expanded) classes.push('expanded');
    if (entering) classes.push('enter');
    parts.push(
      `<tr data-path="${escapeHtml(row.path)}"${classes.length ? ` class="${classes.join(' ')}"` : ''}>
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

/** 结果到达。经典模式下先进队列，由 revealTick 按节奏搬进 rows。 */
function pushRow(row) {
  if (!classicMode) {
    row.painted = true; // 不做入场动画，直接就是"已画过"
    rows.push(row);
    sortRows();
    scheduleRender();
    status.textContent = `已找到 ${rows.length} 个…`;
    return;
  }
  row.painted = false;
  revealQueue.push(row);
  startReveal();
}

function startReveal() {
  if (revealTimer !== null) return;
  revealTimer = setInterval(revealTick, REVEAL_STEP_MS);
  revealTick(); // 第一条不等，立刻出
}

function revealTick() {
  const pending = revealQueue.length;
  if (pending === 0) {
    stopReveal();
    // 队列清空才轮到汇总上场，否则会出现"已完成"和还在往外浮的结果同框。
    if (deferredDone !== null) {
      const payload = deferredDone;
      deferredDone = null;
      applyDone(payload);
    }
    return;
  }
  // 积压越多一次搬得越多：总揭示时长收敛在 REVEAL_BUDGET_MS 以内。
  const step = Math.max(1, Math.ceil((pending * REVEAL_STEP_MS) / REVEAL_BUDGET_MS));
  for (let i = 0; i < step && revealQueue.length > 0; i += 1) {
    rows.push(revealQueue.shift());
  }
  sortRows();
  render();
  status.textContent = `已找到 ${rows.length} 个…`;
}

function stopReveal() {
  if (revealTimer !== null) {
    clearInterval(revealTimer);
    revealTimer = null;
  }
}

function applyDone(event) {
  summary.hidden = false;
  document.getElementById('sum-apps').textContent = String(event.apps);
  document.getElementById('sum-total').textContent = humanSize(event.totalBytes);
  document.getElementById('sum-sum').textContent = humanSize(event.sumBytes);
  document.getElementById('sum-backend').textContent = backendLabel(event.backend);
  document.getElementById('sum-elapsed').textContent = `${event.elapsedMs} ms`;
  status.textContent = rows.length === 0 ? '没有找到应用' : `完成，共 ${rows.length} 个`;
  setScanning(false);
}

async function runScan() {
  if (scanning) return;
  setScanning(true);
  rows = [];
  expandedPaths.clear();
  stopReveal();
  revealQueue = [];
  deferredDone = null;
  render();
  summary.hidden = true;
  status.textContent = '扫描中…';
  backendDisplay.textContent = BACKEND_PENDING;

  const channel = new Channel();
  channel.onmessage = (event) => {
    if (event.type === 'started') {
      // 后端选定的那一刻就发过来，比第一条结果早得多——所以这个括号是真·实时。
      backendDisplay.textContent = backendLabel(event.backend);
      return;
    }
    if (event.type === 'item') {
      // Item 事件的载荷就是 AppRow 本身，type 字段由 serde 打标签注入。
      const { type, ...row } = event;
      pushRow(row);
      return;
    }
    if (event.type === 'done') {
      if (revealQueue.length > 0 || revealTimer !== null) {
        deferredDone = event;
        return;
      }
      applyDone(event);
      return;
    }
    if (event.type === 'error') {
      stopReveal();
      revealQueue = [];
      deferredDone = null;
      // 失败时后端名可能还停在"检测中"，别让它挂着误导人。
      backendDisplay.textContent = '自动（未确定）';
      status.textContent = `失败：${event.message}`;
      setScanning(false);
    }
  };

  const root = rootInput.value.trim();
  try {
    await invoke('scan_apps', {
      // backend 恒为 auto：界面上没有选项，挑选完全交给 core。
      channel,
      request: { roots: root ? [root] : [], backend: 'auto', threads: 0 },
    });
  } catch (error) {
    backendDisplay.textContent = '自动（未确定）';
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

  // 经典模式只影响外观和揭示节奏，不影响任何检测结果，所以中途切换是安全的。
  classicInput.addEventListener('change', () => {
    classicMode = classicInput.checked;
    document.documentElement.classList.toggle('classic', classicMode);
    if (!classicMode) {
      // 关掉时把还在排队的结果一次性放出来，别让它们憋着。
      stopReveal();
      while (revealQueue.length > 0) {
        const row = revealQueue.shift();
        row.painted = true;
        rows.push(row);
      }
      sortRows();
      render();
      if (deferredDone !== null) {
        const payload = deferredDone;
        deferredDone = null;
        applyDone(payload);
      }
    }
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
