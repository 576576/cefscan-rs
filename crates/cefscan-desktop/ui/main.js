// cefscanw 前端：浏览器原生 API + Tauri 注入的 window.__TAURI__ 全局对象
// （tauri.conf.json 里 withGlobalTauri = true）。

const MAX_ITEMS = 500;

/** 折叠路径时前面保留的目录层数（盘符 / UNC 根不计入）。 */
const PATH_HEAD_SEGMENTS = 3;

/** 后端显示文案：前缀固定是"自动"，括号里是具体后端名。 */
const BACKEND_PENDING = '自动（待检测）';
const BACKEND_UNKNOWN = '自动（未确定）';

function backendLabel(name) {
  return `自动（${name}）`;
}

/** 卡片逐张揭示：每拍间隔 150 ms，总时长预算 12000 ms，每拍最多 3 张。 */
const REVEAL_STEP_MS = 150;
const REVEAL_BUDGET_MS = 12000;
const REVEAL_MAX_PER_TICK = 3;

/** 自动跟随的容差：滚到离底部这么近就算"回到底了"，重新开始跟随。 */
const FOLLOW_SLACK = 24;

/** 卡片墙自动跟随的速度上限（px/秒）。 */
const SCROLL_MAX_PX_PER_SEC = 340;
/** 收尾减速的参考距离（px）：剩余不足这么多就按比例放慢。 */
const SCROLL_EASE_PX = 90;
/** 收尾时的最低速度（px/秒）。 */
const SCROLL_MIN_PX_PER_SEC = 70;

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

const picker = document.getElementById('picker');
const classicView = document.getElementById('classic-view');
const toolView = document.getElementById('tool-view');
const cards = document.getElementById('cards');
const classicCount = document.getElementById('classic-count');
const classicRefresh = document.getElementById('classic-refresh');
const body = document.getElementById('results-body');
const empty = document.getElementById('empty');
const startButton = document.getElementById('start-button');
const scanButton = document.getElementById('scan-button');
const rootInput = document.getElementById('root-input');
const backendDisplay = document.getElementById('backend-display');
const summary = document.getElementById('summary');

/** 状态行节点（按 class 选择，不含经典模式那条条数）。 */
const statusNodes = document.querySelectorAll('.status-text');

// 由 tauri.conf.json 的 withGlobalTauri = true 注入；在普通浏览器里打开时为 undefined。
const { invoke, Channel } = window.__TAURI__ ? window.__TAURI__.core : {};

let rows = [];
let sortKey = 'size';
let sortAscending = false;
let renderQueued = false;
let scanning = false;

/** 当前视图：`null` = 初始选择页，`'classic'` = 卡片墙，`'tool'` = 表格。 */
let view = null;

/** 后端 chip 的写入代次，用来丢弃在途的探测结果（见 setBackendLabel）。 */
let backendEpoch = 0;

/** 经典模式下已到达但还没揭示出去的卡片。 */
let revealQueue = [];
let revealTimer = null;
/** 卡片还在往外浮时先压住的汇总，等队列清空再显示。 */
let deferredDone = null;

/** 卡片墙是否跟着最新一行走：用户往上滚就停，滚回底部再恢复。 */
let autoFollow = true;
/** 上一次自动跟随的目标，用来避免重复发起同一个滚动。 */
let lastFollowTarget = -1;
/** 上一次观察到的滚动位置，用来判断这次是往上还是往下。 */
let lastScrollTop = 0;

/** 正在滚向的目标；null = 没在滚。 */
let scrollTarget = null;
let scrollRaf = 0;
let scrollTs = 0;

/** 已展开路径的行。 */
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

/** 用户开了"减少动态效果"就别做平滑滚动和入场动画。 */
function prefersReducedMotion() {
  return (
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

/** 停下自己驱动的滚动动画。 */
function stopScroll() {
  if (scrollRaf) cancelAnimationFrame(scrollRaf);
  scrollRaf = 0;
  scrollTarget = null;
  scrollTs = 0;
}

/** 滚动动画的一帧。 */
function scrollFrame(ts) {
  scrollRaf = 0;
  if (scrollTarget === null) return;
  // 帧间隔夹在 [1, 64] ms；无有效时间戳时用 16 ms 兜底。
  const dt = Number.isFinite(ts) && scrollTs ? Math.min(64, ts - scrollTs) : 16;
  scrollTs = Number.isFinite(ts) ? ts : 0;

  const from = cards.scrollTop;
  const remaining = scrollTarget - from;
  if (Math.abs(remaining) < 0.5) {
    cards.scrollTop = scrollTarget;
    scrollTarget = null;
    return;
  }
  // 上限 + 收尾减速：剩余越少越慢，但有下限。
  const speed = Math.min(
    SCROLL_MAX_PX_PER_SEC,
    Math.max(SCROLL_MIN_PX_PER_SEC, (Math.abs(remaining) / SCROLL_EASE_PX) * SCROLL_MAX_PX_PER_SEC)
  );
  cards.scrollTop = from + Math.sign(remaining) * Math.min(Math.abs(remaining), (speed * dt) / 1000);
  scrollRaf = requestAnimationFrame(scrollFrame);
}

/** 把卡片墙滚到 target，速度不超过 `SCROLL_MAX_PX_PER_SEC`。已经在滚就直接改目标。 */
function scrollCardsTo(target) {
  if (prefersReducedMotion()) {
    stopScroll();
    cards.scrollTop = target;
    return;
  }
  scrollTarget = target;
  if (!scrollRaf) scrollRaf = requestAnimationFrame(scrollFrame);
}

function setStatus(text) {
  for (const node of statusNodes) node.textContent = text;
}

/**
 * 折叠路径：前面只留「根 + 3 层目录」，中间省略号，后面只留文件名。
 *
 *   C:\Users\16695\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe
 *   → C:\Users\16695\AppData\…\WorkBuddy.exe
 */
function foldPath(path) {
  const separator = path.includes('\\') ? '\\' : '/';
  const segments = path.split(separator);
  const head = segments.slice(0, PATH_HEAD_SEGMENTS + 1).join(separator);
  const tail = segments[segments.length - 1];
  const folded = `${head}${separator}…${separator}${tail}`;
  // 折完反而更长时保留原路径。
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

/** 只重画一行的路径单元格。 */
function paintPathCell(row) {
  const cell = row.querySelector('td.path');
  if (!cell) return;
  const path = row.dataset.path;
  const expanded = expandedPaths.has(path);
  row.classList.toggle('expanded', expanded);
  cell.innerHTML = pathCellHtml(path, expanded);
}

/** 按当前排序键返回排好序的副本（工具页用）。 */
function sortedRows() {
  return rows.slice().sort((left, right) => {
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
    // 主键相同时用路径兜底。
    if (order === 0) order = left.path.localeCompare(right.path);
    return sortAscending ? order : -order;
  });
}

/** 按当前视图重画。视图是初始选择页时两个容器都藏着，不用画。 */
function render() {
  renderQueued = false;
  if (view === 'tool') {
    renderTable();
  } else if (view === 'classic') {
    renderCards();
  }
}

/** 流式扫描时每条结果都会触发重绘，用 rAF 合并成每帧最多一次。 */
function scheduleRender() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(render);
}

function renderTable() {
  empty.hidden = rows.length > 0;

  // 只渲染前 MAX_ITEMS 条；排序对副本做，`rows` 本身保持到达顺序。
  const visible = sortedRows().slice(0, MAX_ITEMS);
  const parts = [];
  for (const row of visible) {
    const color = KIND_COLORS[row.kind] || KIND_COLORS.unknown;
    const detail = row.evidence ? `${row.kind} · ${row.evidence}` : row.kind;
    const expanded = expandedPaths.has(row.path);
    const icon = row.icon ? `<img src="${row.icon}" alt="" width="18" height="18" />` : '';
    // 工具模式不做入场动画，但仍标记为已画过。
    row.painted = true;
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

/** 卡片里的图标；取不到图标时留一个同样大小的空槽。 */
function cardIconHtml(row) {
  const icon = row.icon
    ? `<img src="${row.icon}" alt="" width="32" height="32" />`
    : '<span class="placeholder"></span>';
  return row.running ? `${icon}<span class="dot" title="运行中"></span>` : icon;
}

/** 经典模式的条数文案。 */
function classicCountText(total) {
  return `您的电脑里有 ${total} 个 Chromium`;
}

function renderCards() {
  // 条数在顶部区域，每次重绘都刷一次。
  classicCount.textContent = classicCountText(rows.length);

  // 卡片墙按到达顺序排，不做排序。
  const visible = rows.slice(0, MAX_ITEMS);
  const parts = [];
  for (const row of visible) {
    // 只有还没画过的卡片才带 .enter。
    const entering = !row.painted;
    row.painted = true;
    const path = escapeHtml(row.path);
    parts.push(
      `<article class="card${entering ? ' enter' : ''}" data-path="${path}" title="点击在资源管理器中显示：${path}">
        <div class="card-icon">${cardIconHtml(row)}</div>
        <div class="card-name">${escapeHtml(row.name)}</div>
        <div class="card-size">${humanSize(row.size)}</div>
      </article>`
    );
  }
  if (rows.length > visible.length) {
    parts.push(`<p class="cards-more">仅显示前 ${visible.length} 个，共 ${rows.length} 个</p>`);
  }
  cards.innerHTML = parts.join('');
  followNewest();
}

/** 把滚动位置对齐到整行，并平滑跟到最新一行。 */
function followNewest() {
  if (!autoFollow) return;
  const list = cards.querySelectorAll('.card');
  if (list.length === 0) return;

  const style = getComputedStyle(cards);
  const padTop = Number.parseFloat(style.paddingTop) || 0;
  const currentPadBottom = Number.parseFloat(style.paddingBottom) || 0;
  // 首次使用时把 CSS 的底部内边距记为基准。
  if (cards.dataset.basePadBottom === undefined) {
    cards.dataset.basePadBottom = String(currentPadBottom);
  }
  const basePadBottom = Number.parseFloat(cards.dataset.basePadBottom) || 0;

  // 反推出基准内边距下的溢出量。
  const natural = cards.scrollHeight - cards.clientHeight - (currentPadBottom - basePadBottom);
  if (natural <= 0) {
    // 还没溢出一屏，什么都不用做（顺手把可能残留的补量清掉）。
    if (currentPadBottom !== basePadBottom) {
      cards.style.paddingBottom = `${basePadBottom}px`;
    }
    return;
  }

  const gap = Number.parseFloat(style.rowGap) || 0;
  const last = list[list.length - 1];
  const pitch = list[0].offsetHeight + gap;
  if (pitch <= 0) return;

  // 最后一行完整可见所需的最小滚动量，再往上对齐到行边界。
  const lastBottom = last.offsetTop + last.offsetHeight;
  const minTop = Math.max(0, lastBottom - cards.clientHeight);
  const steps = Math.max(0, Math.ceil((minTop - padTop) / pitch));
  const target = Math.max(cards.scrollTop, padTop + steps * pitch);

  const wantedPadBottom = Math.max(basePadBottom, basePadBottom + target - natural);
  if (Math.abs(wantedPadBottom - currentPadBottom) > 0.5) {
    cards.style.paddingBottom = `${wantedPadBottom}px`;
  }

  if (target === lastFollowTarget) return;
  lastFollowTarget = target;
  scrollCardsTo(target);
}

/** 新一次扫描开始时把跟随状态归零。 */
function resetFollow() {
  autoFollow = true;
  lastFollowTarget = -1;
  lastScrollTop = 0;
  stopScroll();
  cards.scrollTop = 0;
}

function setScanning(value) {
  scanning = value;
  // 工具模式的"扫描"按钮：文案在"开始扫描 / 扫描中…"之间切，扫描时禁用。
  scanButton.disabled = value;
  scanButton.textContent = value ? '扫描中…' : '开始扫描';
  // 刷新是图标按钮，单独设 disabled（不能走 textContent）。
  classicRefresh.disabled = value;
}

/** 切换视图。 */
function showView(next) {
  view = next;
  picker.hidden = next !== null;
  classicView.hidden = next !== 'classic';
  toolView.hidden = next !== 'tool';
  document.documentElement.classList.toggle('classic', next === 'classic');

  if (next === 'tool') void refreshBackend();
  if (next !== null) render();
}

/** 探测与扫描共用的请求参数。 */
function currentRequest() {
  const root = rootInput.value.trim();
  // backend 恒为 auto。
  return { roots: root ? [root] : [], backend: 'auto', threads: 0 };
}

/** 写后端 chip 的唯一入口；每次写入把 backendEpoch 加一。 */
function setBackendLabel(text) {
  backendEpoch += 1;
  backendDisplay.textContent = text;
}

/** 重新探测这次扫描会用哪个后端，把结果写进工具栏的 chip。 */
async function refreshBackend() {
  if (!invoke) return;
  setBackendLabel(BACKEND_PENDING);
  const epoch = backendEpoch;
  try {
    const probe = await invoke('detect_backend', { request: currentRequest() });
    if (epoch === backendEpoch) setBackendLabel(backendLabel(probe.backend));
  } catch (error) {
    if (epoch === backendEpoch) setBackendLabel(BACKEND_UNKNOWN);
  }
}

/** 结果到达。经典模式下先进队列，由 revealTick 按节奏搬进 rows。 */
function pushRow(row) {
  if (view !== 'classic') {
    row.painted = true; // 表格不做入场动画，直接就是"已画过"
    rows.push(row);
    scheduleRender();
    setStatus(`已找到 ${rows.length} 个…`);
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
    // 队列清空后才轮到汇总。
    if (deferredDone !== null) {
      const payload = deferredDone;
      deferredDone = null;
      applyDone(payload);
    }
    return;
  }
  // 按预算算每拍搬运量，但不超过 REVEAL_MAX_PER_TICK。
  const byBudget = Math.max(1, Math.ceil((pending * REVEAL_STEP_MS) / REVEAL_BUDGET_MS));
  const step = Math.min(byBudget, REVEAL_MAX_PER_TICK);
  for (let i = 0; i < step && revealQueue.length > 0; i += 1) {
    rows.push(revealQueue.shift());
  }
  render();
  setStatus(`已找到 ${rows.length} 个…`);
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
  // 状态行只服务选择页和工具模式；经典模式的条数由 renderCards 负责。
  setStatus(
    rows.length === 0
      ? '没有找到应用'
      : `完成，共 ${rows.length} 个 · 合计 ${humanSize(event.totalBytes)}`
  );
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
  resetFollow();
  render();
  summary.hidden = true;
  setStatus('扫描中…');

  const channel = new Channel();
  channel.onmessage = (event) => {
    if (event.type === 'started') {
      // 后端选定那一刻发来的 started 事件，写入 chip。
      setBackendLabel(backendLabel(event.backend));
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
      // 失败时把后端名置为未确定。
      setBackendLabel(BACKEND_UNKNOWN);
      setStatus(`失败：${event.message}`);
      setScanning(false);
    }
  };

  try {
    await invoke('scan_apps', { channel, request: currentRequest() });
  } catch (error) {
    setBackendLabel(BACKEND_UNKNOWN);
    setStatus(`失败：${String(error)}`);
    setScanning(false);
  }
}

function bootstrap() {
  if (!window.__TAURI__ || !window.__TAURI__.core) {
    setStatus('未检测到 Tauri 运行时');
    startButton.disabled = true;
    scanButton.disabled = true;
    return;
  }

  // 初始选择页：选好模式再进去；已有结果时只切画法，不重扫。
  startButton.addEventListener('click', () => {
    const chosen = document.querySelector('input[name="mode"]:checked');
    const next = chosen && chosen.value === 'tool' ? 'tool' : 'classic';
    showView(next);

    if (rows.length > 0 || scanning) return;
    // 没有结果时，经典模式顺手开扫；工具模式不自动扫。
    if (next === 'classic') void runScan();
  });

  // 两个视图的"返回"按钮：只切视图，不打断正在跑的扫描。
  for (const id of ['classic-back', 'tool-back']) {
    document.getElementById(id).addEventListener('click', () => showView(null));
  }

  // 经典模式的刷新胶囊 = 重扫一次。
  classicRefresh.addEventListener('click', () => void runScan());

  scanButton.addEventListener('click', () => void runScan());
  backendDisplay.addEventListener('click', () => void refreshBackend());
  rootInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') void runScan();
  });

  // 点一张卡片 = 在资源管理器中定位它。
  cards.addEventListener('click', (event) => {
    const card = event.target.closest('.card');
    if (!card || !card.dataset.path) return;
    invoke('reveal', { path: card.dataset.path }).catch((error) => {
      setStatus(`无法定位：${String(error)}`);
    });
  });

  // 自动跟随的开关：按滚动方向判断用户是否在往上滚；lastScrollTop 只在这里写。
  cards.addEventListener('scroll', () => {
    const top = cards.scrollTop;
    if (top < lastScrollTop - 2) {
      autoFollow = false;
      lastFollowTarget = -1;
      // 用户自己翻页时停掉正在跑的自动跟随动画。
      stopScroll();
    }
    const overflow = cards.scrollHeight - cards.clientHeight;
    if (overflow <= 0 || top >= overflow - FOLLOW_SLACK) autoFollow = true;
    lastScrollTop = top;
  });

  // 卡片墙尺寸变化时重新对齐一次。
  new ResizeObserver(() => {
    lastFollowTarget = -1;
    followNewest();
  }).observe(cards);

  body.addEventListener('click', (event) => {
    // "在资源管理器中显示"按钮：拦下来，避免顺带收起行。
    const reveal = event.target.closest('[data-reveal]');
    if (reveal) {
      event.stopPropagation();
      invoke('reveal', { path: reveal.dataset.reveal }).catch((error) => {
        setStatus(`无法定位：${String(error)}`);
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
      // 只重画：sortKey / sortAscending 变了，renderTable 会重排一份副本。
      render();
    });
  }

  render();
}

bootstrap();
