// Clerk GUI 前端：通过 Tauri 命令/事件与 clerk-core 后端交互。
// 依赖 tauri.conf.json 中的 withGlobalTauri 注入的 window.__TAURI__，无需打包工具。

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const chatEl = document.getElementById("chat");
const inputEl = document.getElementById("input");
const sendBtn = document.getElementById("send");
const statusEl = document.getElementById("status");
const attachEl = document.getElementById("attachments");

let streaming = false;
let currentBubble = null; // 当前流式助手气泡的内容节点
let currentThink = null; // 当前流式推理内容节点
let loadingBubble = null; // 首个输出块到达前的加载气泡
let streamStartedAt = null; // 本轮生成开始时间（ms 时间戳）
let statusTimer = null; // 状态栏与加载动画的定时器
let spinnerIdx = 0;
const runningTools = new Set(); // 正在执行中的工具名

const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DOTS_FRAMES = ["... ", ".. .", ". ..", " ..."];

function scrollToBottom() {
  chatEl.scrollTop = chatEl.scrollHeight;
}

function setStatus(text) {
  statusEl.textContent = text;
}

// 流式期间的状态栏：spinner + 动画省略号 + 已耗时 + 正在执行的工具
function renderStreamingStatus() {
  if (!streaming || streamStartedAt === null) {
    return;
  }
  const spinner = SPINNER_FRAMES[spinnerIdx % SPINNER_FRAMES.length];
  const dots = DOTS_FRAMES[Math.floor(spinnerIdx / 2) % DOTS_FRAMES.length];
  const elapsed = Math.floor((Date.now() - streamStartedAt) / 1000);
  let text = `${spinner} 思考中${dots} (${elapsed}s)`;
  if (runningTools.size > 0) {
    text += ` · 正在执行工具: ${[...runningTools].join(", ")}`;
  }
  setStatus(text);
}

function updateLoadingElapsed() {
  if (loadingBubble && streamStartedAt !== null) {
    const elapsed = Math.floor((Date.now() - streamStartedAt) / 1000);
    loadingBubble.querySelector(".loading-elapsed").textContent = `${elapsed}s`;
  }
}

// 首个输出块到达前的加载气泡：CSS 跳动的三点 + 已耗时秒数
function beginLoadingBubble() {
  removeLoadingBubble();
  const div = document.createElement("div");
  div.className = "msg assistant loading";
  const dots = document.createElement("span");
  dots.className = "typing-dots";
  for (let i = 0; i < 3; i++) {
    dots.appendChild(document.createElement("span"));
  }
  const elapsed = document.createElement("span");
  elapsed.className = "loading-elapsed";
  elapsed.textContent = "0s";
  div.appendChild(dots);
  div.appendChild(elapsed);
  chatEl.appendChild(div);
  loadingBubble = div;
  scrollToBottom();
}

function removeLoadingBubble() {
  if (loadingBubble) {
    loadingBubble.remove();
    loadingBubble = null;
  }
}

// 开始一轮生成：加载气泡 + 状态栏动画定时器
function startStreamUi() {
  streamStartedAt = Date.now();
  spinnerIdx = 0;
  beginLoadingBubble();
  renderStreamingStatus();
  statusTimer = setInterval(() => {
    spinnerIdx++;
    renderStreamingStatus();
    updateLoadingElapsed();
  }, 100);
}

function stopStreamUi() {
  if (statusTimer) {
    clearInterval(statusTimer);
    statusTimer = null;
  }
  streamStartedAt = null;
  runningTools.clear();
  removeLoadingBubble();
}

function addMessage(role, content) {
  const div = document.createElement("div");
  div.className = `msg ${role}`;
  div.textContent = content;
  chatEl.appendChild(div);
  scrollToBottom();
  return div;
}

// 历史消息中的 <think>...</think> 渲染为可折叠的思考块
function addAssistantMessage(content) {
  const div = document.createElement("div");
  div.className = "msg assistant";
  const parts = content.split(/<\/?think>/);
  let inThink = false;
  for (const part of parts) {
    if (!part) {
      inThink = !inThink;
      continue;
    }
    if (inThink) {
      div.appendChild(makeThinkBlock(part));
    } else {
      const span = document.createElement("span");
      span.textContent = part;
      div.appendChild(span);
    }
    inThink = !inThink;
  }
  chatEl.appendChild(div);
  scrollToBottom();
  return div;
}

function makeThinkBlock(text) {
  const details = document.createElement("details");
  details.className = "think";
  const summary = document.createElement("summary");
  summary.textContent = "思考过程";
  const body = document.createElement("div");
  body.className = "think-body";
  body.textContent = text;
  details.appendChild(summary);
  details.appendChild(body);
  return { details, body };
}

function beginStreamBubble() {
  const div = document.createElement("div");
  div.className = "msg assistant";
  const body = document.createElement("span");
  div.appendChild(body);
  chatEl.appendChild(div);
  currentBubble = body;
  currentThink = null;
  scrollToBottom();
}

function appendChunk(payload) {
  // 首个输出块到达：撤下加载气泡，换成真正的流式气泡
  removeLoadingBubble();
  if (!currentBubble) {
    beginStreamBubble();
  }
  if (payload.reasoning) {
    if (!currentThink) {
      currentThink = makeThinkBlock("");
      currentBubble.parentElement.insertBefore(currentThink.details, currentBubble);
    }
    currentThink.body.textContent += payload.reasoning;
  }
  if (payload.content) {
    currentBubble.textContent += payload.content;
  }
  scrollToBottom();
}

function addFileChip(payload) {
  const chip = document.createElement("div");
  chip.className = "file-chip";

  const icon = document.createElement("span");
  icon.textContent = "[文件]";
  const name = document.createElement("span");
  name.className = "file-name";
  name.textContent = payload.name;

  const saveBtn = document.createElement("button");
  saveBtn.textContent = "保存";
  saveBtn.addEventListener("click", async () => {
    try {
      const dest = await invoke("save_file", { path: payload.path });
      setStatus(`已保存到 ${dest}`);
    } catch (e) {
      setStatus(`保存失败: ${e}`);
    }
  });

  const saveAsBtn = document.createElement("button");
  saveAsBtn.textContent = "另存为";
  saveAsBtn.addEventListener("click", async () => {
    try {
      const dest = await invoke("save_file_as", { path: payload.path });
      setStatus(dest ? `已保存到 ${dest}` : "已取消保存");
    } catch (e) {
      setStatus(`保存失败: ${e}`);
    }
  });

  chip.appendChild(icon);
  chip.appendChild(name);
  chip.appendChild(saveBtn);
  chip.appendChild(saveAsBtn);
  chatEl.appendChild(chip);
  scrollToBottom();
}

function showApproval(payload) {
  const div = document.createElement("div");
  div.className = "approval";

  const text = document.createElement("div");
  const args = JSON.stringify(payload.arguments);
  text.textContent = `工具 ${payload.name} 请求执行，参数: ${args}`;
  div.appendChild(text);

  const actions = document.createElement("div");
  actions.className = "approval-actions";

  const answer = async (approved) => {
    try {
      await invoke("respond_approval", { approved });
      actions.textContent = approved ? "已批准" : "已拒绝";
      actions.className = "approval-result";
    } catch (e) {
      setStatus(`审批响应失败: ${e}`);
    }
  };

  const approveBtn = document.createElement("button");
  approveBtn.textContent = "批准";
  approveBtn.addEventListener("click", () => answer(true));

  const rejectBtn = document.createElement("button");
  rejectBtn.textContent = "拒绝";
  rejectBtn.className = "reject";
  rejectBtn.addEventListener("click", () => answer(false));

  actions.appendChild(approveBtn);
  actions.appendChild(rejectBtn);
  div.appendChild(actions);
  chatEl.appendChild(div);
  scrollToBottom();
}

function addAttachmentChip(payload) {
  const chip = document.createElement("span");
  chip.className = "attachment-chip";
  const label = payload.kind === "video" ? "[视频]" : "[图片]";
  chip.textContent = `${label} ${payload.name}`;
  if (payload.warning) {
    chip.textContent += `（${payload.warning}）`;
  }
  attachEl.appendChild(chip);
}

function clearAttachmentChips() {
  attachEl.textContent = "";
}

function bufToBase64(buf) {
  const bytes = new Uint8Array(buf);
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunkSize));
  }
  return btoa(binary);
}

async function handlePaste(event) {
  const items = event.clipboardData ? event.clipboardData.items : [];
  const files = [];
  for (const item of items) {
    if (item.kind === "file") {
      const file = item.getAsFile();
      if (file) files.push(file);
    }
  }
  if (files.length === 0) {
    return; // 普通文本粘贴走默认行为
  }
  event.preventDefault();

  for (const file of files) {
    try {
      const buf = await file.arrayBuffer();
      const payload = await invoke("attach_media", {
        name: file.name || "",
        mime: file.type || "",
        data: bufToBase64(buf),
      });
      addAttachmentChip(payload);
      setStatus(`已添加附件: ${payload.name}`);
    } catch (e) {
      setStatus(`附件失败: ${e}`);
    }
  }
}

async function send() {
  const text = inputEl.value.trim();
  if (!text || streaming) {
    return;
  }
  inputEl.value = "";
  streaming = true;
  sendBtn.disabled = true;

  addMessage("user", text);
  clearAttachmentChips();
  startStreamUi();

  try {
    await invoke("send_message", { message: text });
  } catch (e) {
    stopStreamUi();
    setStatus(`发送失败: ${e}`);
    streaming = false;
    sendBtn.disabled = false;
  }
}

sendBtn.addEventListener("click", send);
inputEl.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    send();
  }
});
inputEl.addEventListener("paste", handlePaste);

listen("clerk-chunk", (event) => appendChunk(event.payload));

listen("clerk-tool", (event) => {
  const text = event.payload.text;
  addMessage("tool", text);
  // 跟踪正在执行的工具，在状态栏给出执行指示（与 format_tool_event 的输出对应）
  const call = text.match(/^调用工具 ([^:]+):/);
  const result = text.match(/^工具 ([^ ]+) 结果:/);
  if (call) {
    runningTools.add(call[1].trim());
  } else if (result) {
    runningTools.delete(result[1].trim());
  } else if (text.startsWith("工具错误")) {
    runningTools.clear();
  }
  renderStreamingStatus();
});

listen("clerk-approval", (event) => showApproval(event.payload));

listen("clerk-file", (event) => addFileChip(event.payload));

listen("clerk-done", (event) => {
  const { ok, reply, error } = event.payload;
  stopStreamUi();
  if (ok && currentBubble && !currentBubble.textContent && !currentThink) {
    // 流式路径没有产出任何块时，用最终回复填充气泡
    currentBubble.textContent = reply;
  } else if (ok && !currentBubble && reply) {
    // 非流式路径（模型不支持流式）没有任何块，直接展示最终回复
    addAssistantMessage(reply);
  }
  if (!ok) {
    if (currentBubble && !currentBubble.textContent && !currentThink) {
      currentBubble.parentElement.remove();
    }
    addMessage("tool error", `处理失败: ${error || "未知错误"}`);
    setStatus("出错了");
  } else {
    setStatus("就绪");
  }
  currentBubble = null;
  currentThink = null;
  streaming = false;
  sendBtn.disabled = false;
  scrollToBottom();
});

async function loadHistory() {
  try {
    const messages = await invoke("get_history");
    for (const msg of messages) {
      if (msg.role === "assistant") {
        addAssistantMessage(msg.content);
      } else {
        addMessage(msg.role, msg.content);
      }
    }
  } catch (e) {
    setStatus(`读取历史失败: ${e}`);
  }
}

loadHistory();
