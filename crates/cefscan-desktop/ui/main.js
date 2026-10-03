// cefscanw 前端。刻意不引入任何打包工具：这里只有浏览器原生 API +
// Tauri 注入的 window.__TAURI__ 全局对象（tauri.conf.json 里 withGlobalTauri = true），
// 所以整个 GUI 用 `cargo build --release` 就能产出，不需要 Node/npm。
//
// 三个视图共用一个数据源 `rows`：扫描结果永远往 `rows` 里堆，表格和卡片墙各自
// 只是它的一种画法。这样切视图不需要重扫，也不会出现"两个视图各记一份、慢慢对不上"。

const MAX_ITEMS = 500;

/** 折叠路径时前面保留的目录层数（盘符 / UNC 根不计入）。 */
const PATH_HEAD_SEGMENTS = 3;

/**
 * 后端显示文案。后端是自动挑的（有索引服务就用，没有就自己遍历），
 * 所以前缀固定是"自动"；真正有信息量的是括号里的**具体后端名**
 * （`cefscan` / `Everything`），而不是"遍历"/"索引"这类内部叫法。
 */
const BACKEND_PENDING = '自动（待检测）';
const BACKEND_UNKNOWN = '自动（未确定）';

function backendLabel(name) {
  return `自动（${name}）`;
}

/**
 * 经典模式卡片"缓缓浮现"的节奏。
 *
 * 为什么要排队而不是收到就画：索引后端会在几百毫秒内一次吐出几十条结果，
 * 如果直接画出来，用户看到的是一整面墙同时"啪"地出现——只有遍历后端那种
 * 天然一条条到达的节奏才自带"缓缓出现"的观感。排队 + 固定节奏让两种后端
 * 看起来一致。
 *
 * REVEAL_BUDGET_MS 是止损：500 条按 120ms 一条要一分钟，所以积压越多
 * 一次揭示得越多，总时长收敛在这个预算内（见 revealTick 的 step 计算）。
 */
const REVEAL_STEP_MS = 120;
const REVEAL_BUDGET_MS = 4000;

/** 自动跟随的容差：滚到离底部这么近就算"回到底了"，重新开始跟随。 */
const FOLLOW_SLACK = 24;

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

/**
 * 状态行按 class 一把改掉，但**不含经典模式那条条数**——它有自己的文案格式
 * （见 renderCards），挂在 `#classic-count` 上，不是 `.status-text`。
 */
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

/**
 * 卡片墙是否跟着最新一行走。
 *
 * 用户往上滚就停（他在翻看旧结果，别跟他抢滚动条），滚回底部再自动恢复。
 */
let autoFollow = true;
/** 上一次自动跟随的目标，用来避免重复发起同一个平滑滚动。 */
let lastFollowTarget = -1;
/** 上一次观察到的滚动位置，只用来判断"这次是往上还是往下"。 */
let lastScrollTop = 0;

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

/** 用户开了"减少动态效果"就别做平滑滚动和入场动画。 */
function prefersReducedMotion() {
  return (
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

function setStatus(text) {
  for (const node of statusNodes) node.textContent = text;
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

/** 按当前视图重画。视图是初始选择页时两个容器都藏着，不用画。 */
function render() {
  renderQueued = false;
  if (view === 'tool') {
    renderTable();
  } else if (view === 'classic') {
    renderCards();
  }
  // view === null（初始选择页）时什么都不画。这种状态下到达的结果已经由 pushRow
  // 标成 painted、直接进了 rows，所以等用户再进某个视图时是一次画完、不补动画——
  // 他本来也没在看，没必要让两百张卡片一起演一遍入场。
}

/** 流式扫描时每条结果都会触发重绘，用 rAF 合并成每帧最多一次。 */
function scheduleRender() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(render);
}

function renderTable() {
  empty.hidden = rows.length > 0;

  // 全盘扫描可能有上千条结果，只渲染前 MAX_ITEMS 条，避免 DOM 过大拖慢 WebView。
  const visible = rows.slice(0, MAX_ITEMS);
  const parts = [];
  for (const row of visible) {
    const color = KIND_COLORS[row.kind] || KIND_COLORS.unknown;
    const detail = row.evidence ? `${row.kind} · ${row.evidence}` : row.kind;
    const expanded = expandedPaths.has(row.path);
    const icon = row.icon ? `<img src="${row.icon}" alt="" width="18" height="18" />` : '';
    // 工具模式不做入场动画（"缓缓浮现"是经典模式卡片墙的事），但标记还是要打上：
    // 之后切到经典模式时，这些行不该再演一遍入场。
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

/** 卡片里的图标。取不到图标时留一个同样大小的空槽，卡片高度才不会参差不齐。 */
function cardIconHtml(row) {
  const icon = row.icon
    ? `<img src="${row.icon}" alt="" width="40" height="40" />`
    : '<span class="placeholder"></span>';
  return row.running ? `${icon}<span class="dot" title="运行中"></span>` : icon;
}

/** 经典模式的条数文案。0 也照实说，所以不需要单独的空态。 */
function classicCountText(total) {
  return `您的电脑里有 ${total} 个 Chromium`;
}

function renderCards() {
  // 条数在**顶部区域**（喜报"喜报"两字正下方），不在滚动区里，所以每次重绘都刷一次。
  classicCount.textContent = classicCountText(rows.length);

  const visible = rows.slice(0, MAX_ITEMS);
  const parts = [];
  for (const row of visible) {
    // 同表格：只有还没画过的卡片才带 .enter，整墙重绘不会让老卡片重放动画。
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

/**
 * 把滚动位置对齐到整行，并平滑跟到最新一行。
 *
 * 三件事必须一起做，少一件都会看出破绽：
 *
 * 1. **量行距用 offsetHeight，不用 getBoundingClientRect**。新卡片正带着入场
 *    动画（`translateY(10px) scale(0.96)`），rect 返回的是**动画中的**几何，
 *    scale 会把 116px 的卡片量成 111px，行距随之算小、对齐全偏。offset* 是
 *    布局值，不受 transform 影响。
 * 2. **对齐要带上 padding-top**。行顶边在 `padding-top + k * 行距` 处，按纯
 *    `k * 行距` 对齐的话视口顶部会切掉小半行。顶部区域（图标胶囊 + 条数）现在挪到
 *    了 `#cards` 外面，所以这个 padding-top 是 0——但公式照旧得带上它，
 *    否则以后谁再往里加内边距就又错了。
 * 3. **底部内边距补足**，让最大滚动量正好等于对齐后的目标位置。不补的话目标
 *    超过最大滚动量会被浏览器夹回去，对齐白做——而且最后一行（正是"自动跟随
 *    最新"最该看清的那一行）会被视口底部切掉一截。补出来的量小于一个行距，
 *    又落在最后一行下方，视觉上看不出来。
 *
 * 目标是"最后一行完整可见 + 视口顶部是行顶边"这两个条件的**最小**解，所以
 * 内容每多一行，目标正好前进一个行距：看上去就是整行整行地往上走。
 */
function followNewest() {
  if (!autoFollow) return;
  const list = cards.querySelectorAll('.card');
  if (list.length === 0) return;

  const style = getComputedStyle(cards);
  const padTop = Number.parseFloat(style.paddingTop) || 0;
  const currentPadBottom = Number.parseFloat(style.paddingBottom) || 0;
  // 首次使用时把 CSS 里的底部内边距记下来当基准，免得 JS 和 CSS 各写一个数值、
  // 改了其中一边对不上。基准只认第一次读到的值。
  if (cards.dataset.basePadBottom === undefined) {
    cards.dataset.basePadBottom = String(currentPadBottom);
  }
  const basePadBottom = Number.parseFloat(cards.dataset.basePadBottom) || 0;

  // 反推出"基准内边距下"的溢出量。这样它就跟当前 padding-bottom 无关了，
  // 否则补一次内边距会改变 scrollHeight，下次再算又要改回去，来回震荡。
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
  cards.scrollTo({ top: target, behavior: prefersReducedMotion() ? 'auto' : 'smooth' });
}

/** 新一次扫描开始时把跟随状态归零。 */
function resetFollow() {
  autoFollow = true;
  lastFollowTarget = -1;
  lastScrollTop = 0;
  cards.scrollTop = 0;
}

function setScanning(value) {
  scanning = value;
  for (const node of [startButton, scanButton]) {
    node.disabled = value;
    node.textContent = value ? '扫描中…' : '开始扫描';
  }
  // 图标按钮不能走上面那个循环——`textContent = …` 会把图标本身抹掉。
  classicRefresh.disabled = value;
}

/**
 * 切换视图。
 *
 * 只有经典模式用喜报皮肤；初始选择页和工具模式都是深色。所以 html 上开局不带
 * 任何主题类（深色是 `:root` 的默认值），进经典模式才加上——也就不存在"脚本跑
 * 起来之前闪一下"的问题。
 */
function showView(next) {
  view = next;
  picker.hidden = next !== null;
  classicView.hidden = next !== 'classic';
  toolView.hidden = next !== 'tool';
  document.documentElement.classList.toggle('classic', next === 'classic');

  if (next === 'tool') void refreshBackend();
  if (next !== null) render();
}

/** 探测和扫描必须用**同一份参数**，否则 chip 上写的和结果里报的会对不上。 */
function currentRequest() {
  const root = rootInput.value.trim();
  // backend 恒为 auto：界面上没有选项，挑选完全交给 core。
  return { roots: root ? [root] : [], backend: 'auto', threads: 0 };
}

/**
 * 写后端 chip 的唯一入口：**谁最后调用谁说了算**。
 *
 * 每次写入都把 epoch +1，`refreshBackend` 在 await 回来之后对不上号就丢弃自己的
 * 结果。没有这道闸的话，一次在途的探测会在扫描已经失败之后把"未确定"又盖回成一个
 * 后端名——用户看到的是"失败了，但后端是 cefscan"，自相矛盾。
 */
function setBackendLabel(text) {
  backendEpoch += 1;
  backendDisplay.textContent = text;
}

/**
 * 重新探测"这次扫描会用哪个后端"，把结果写进工具栏那个 chip。
 *
 * 进工具模式时探一次、用户点一下 chip 再探一次——**不等点"开始扫描"**。
 * "自动"要是个可信的选项，就得在开扫之前就能看到它选了谁。
 */
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
    sortRows();
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
  // 这条状态行只服务选择页和工具模式。经典模式的结果显示是顶部那行条数
  // （`renderCards` 负责），文案格式不一样，所以不在这里管它。
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
      // 后端选定那一刻就发过来，比第一条结果早得多。进工具模式时已经探过一次，
      // 所以这里通常是同一个值，作用只是把"探测"坐实成"实际用的"。
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
      // 失败时后端名可能还停在"待检测"，别让它挂着误导人。
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

  // 初始选择页：选好模式再点开始扫描，扫描在选中的视图里跑。
  startButton.addEventListener('click', () => {
    const chosen = document.querySelector('input[name="mode"]:checked');
    showView(chosen && chosen.value === 'tool' ? 'tool' : 'classic');
    void runScan();
  });

  // 两个视图各自的"返回"：只切视图，**不打断正在跑的扫描**——结果照旧往 rows 里堆，
  // 回到哪个视图都能看到。
  for (const id of ['classic-back', 'tool-back']) {
    document.getElementById(id).addEventListener('click', () => showView(null));
  }

  // 经典模式那个刷新胶囊 = 重扫一次（它取代了原来显示结果条数的那个胶囊）。
  classicRefresh.addEventListener('click', () => void runScan());

  scanButton.addEventListener('click', () => void runScan());
  backendDisplay.addEventListener('click', () => void refreshBackend());
  rootInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') void runScan();
  });

  // 卡片墙：点一张卡片 = 在资源管理器中定位它。用事件委托，整墙重绘不会丢监听。
  cards.addEventListener('click', (event) => {
    const card = event.target.closest('.card');
    if (!card || !card.dataset.path) return;
    invoke('reveal', { path: card.dataset.path }).catch((error) => {
      setStatus(`无法定位：${String(error)}`);
    });
  });

  // 自动跟随的开关：只有"往上滚"才可能是用户干的（自动跟随永远向下滚），
  // 所以按方向判断就够了，不用去区分平滑滚动的中间帧，也用不着 scrollend。
  cards.addEventListener('scroll', () => {
    const top = cards.scrollTop;
    if (top < lastScrollTop - 2) {
      autoFollow = false;
      lastFollowTarget = -1;
    }
    const overflow = cards.scrollHeight - cards.clientHeight;
    if (overflow <= 0 || top >= overflow - FOLLOW_SLACK) autoFollow = true;
    lastScrollTop = top;
  });

  // 卡片墙的盒子一变（拖窗口、分屏、WebView 自己改尺寸），最大滚动量和"一行放几个"
  // 都变了：原来的对齐作废，而且浏览器会把 scrollTop 夹回新的最大滚动量——视口顶部
  // 就落在行中间了。所以重新对一次。
  //
  // 用 ResizeObserver 而不是 window 的 resize 事件：盯的是 #cards 自己的盒子，
  // 覆盖面更广，而且回调本来就是按帧合并的，拖动窗口时不会每个像素都滚一下。
  // 改 padding-bottom 不会反过来触发它——#cards 的高度由 flex 决定，内边距变了
  // 盒子尺寸也不变，所以不会自己喂自己。
  new ResizeObserver(() => {
    lastFollowTarget = -1;
    followNewest();
  }).observe(cards);

  body.addEventListener('click', (event) => {
    // 展开后才出现的"在资源管理器中显示"按钮：拦下来，别让它顺带收起行。
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
      sortRows();
      render();
    });
  }

  render();
}

bootstrap();
