import "./styles.css";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  api,
  onProgress,
  type BookInfo,
  type Chapter,
  type Progress,
  type VoiceConfig,
} from "./api";

// ---------- voices (Kokoro) ----------
const VOICES: { id: string; label: string; lang: string }[] = [
  { id: "bm_george", label: "George — British male", lang: "b" },
  { id: "bm_lewis", label: "Lewis — British male", lang: "b" },
  { id: "bf_emma", label: "Emma — British female", lang: "b" },
  { id: "bf_isabella", label: "Isabella — British female", lang: "b" },
  { id: "am_michael", label: "Michael — American male", lang: "a" },
  { id: "am_adam", label: "Adam — American male", lang: "a" },
  { id: "af_heart", label: "Heart — American female", lang: "a" },
  { id: "af_bella", label: "Bella — American female", lang: "a" },
];

type Stage = "drop" | "detect" | "review" | "run" | "done";

interface State {
  stage: Stage;
  book: BookInfo | null;
  models: string[];
  model: string;
  chapters: Chapter[];
  voice: VoiceConfig;
  title: string;
  author: string;
  outDir: string;
  busy: boolean;
  error: string | null;
  progress: Progress | null;
  log: { t: string; msg: string }[];
  resultPath: string | null;
  ollamaUp: boolean;
  ollamaChecked: boolean; // false until the first models poll resolves
  loadingFile: string | null; // filename shown while inspecting a PDF
  polish: boolean; // LLM polish pass over transcripts (on by default; opt-out)
}

const state: State = {
  stage: "drop",
  book: null,
  models: [],
  model: "",
  chapters: [],
  voice: { voice: "bm_george", lang: "b", speed: 1.0 },
  title: "",
  author: "",
  outDir: "",
  busy: false,
  error: null,
  progress: null,
  log: [],
  resultPath: null,
  ollamaUp: false,
  ollamaChecked: false,
  loadingFile: null,
  polish: true,
};

const app = document.querySelector<HTMLDivElement>("#app")!;

// ---------- helpers ----------
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!)
  );
}

function logLine(msg: string) {
  const t = new Date().toLocaleTimeString();
  state.log.push({ t, msg });
  if (state.log.length > 400) state.log.shift();
}

function pickModelDefault(models: string[]): string {
  // Prefer small/fast instruct models for structured extraction.
  const pref = ["gemma4:e2b", "gemma4:latest", "llama3.2", "qwen2.5", "gemma"];
  for (const p of pref) {
    const hit = models.find((m) => m.startsWith(p));
    if (hit) return hit;
  }
  return models[0] ?? "";
}

async function refreshModels() {
  try {
    const models = await api.listModels();
    state.ollamaUp = true;
    state.models = models;
    if (!state.model || !models.includes(state.model)) {
      state.model = pickModelDefault(models);
    }
  } catch {
    state.ollamaUp = false;
    state.models = [];
  } finally {
    state.ollamaChecked = true;
  }
}

// ---------- actions ----------
async function loadPdf(path: string) {
  if (!path.toLowerCase().endsWith(".pdf")) {
    state.error = "Drop a PDF file. Other formats aren’t supported yet.";
    render();
    return;
  }
  state.busy = true;
  state.error = null;
  state.loadingFile = path.slice(path.lastIndexOf("/") + 1);
  render();
  try {
    const book = await api.inspectPdf(path);
    state.book = book;
    state.title = book.file_name.replace(/\.pdf$/i, "");
    state.author = "";
    // Default output dir: a sibling folder next to the PDF.
    const dir = path.slice(0, path.lastIndexOf("/"));
    state.outDir = `${dir}/${state.title} - Audiobook`;
    state.stage = "detect";
  } catch (e) {
    state.error = `Couldn’t read the PDF. ${String(e)}`;
  } finally {
    state.busy = false;
    state.loadingFile = null;
    render();
  }
}

async function detect() {
  if (!state.book) return;
  if (!state.model) {
    state.error = "No Ollama model available. Start Ollama and pull a model.";
    render();
    return;
  }
  state.busy = true;
  state.error = null;
  state.progress = null;
  render();
  try {
    const chapters = await api.detectChapters(state.book.path, state.model);
    state.chapters = chapters;
    state.stage = "review";
  } catch (e) {
    state.error = `Chapter detection failed. ${String(e)}`;
  } finally {
    state.busy = false;
    state.progress = null;
    render();
  }
}

async function generate() {
  if (!state.book) return;
  state.busy = true;
  state.error = null;
  state.stage = "run";
  state.log = [];
  state.resultPath = null;
  render();
  try {
    const path = await api.generate({
      pdf_path: state.book.path,
      out_dir: state.outDir,
      chapters: state.chapters,
      voice: state.voice,
      book_title: state.title,
      author: state.author,
      polish: state.polish,
      polish_model: state.model || null,
    });
    state.resultPath = path;
    state.stage = "done";
  } catch (e) {
    state.error = `Generation failed. ${String(e)}`;
    state.stage = "review";
  } finally {
    state.busy = false;
    render();
  }
}

function reset() {
  state.stage = "drop";
  state.book = null;
  state.chapters = [];
  state.error = null;
  state.progress = null;
  state.log = [];
  state.resultPath = null;
  render();
}

async function browse() {
  const sel = await open({
    multiple: false,
    filters: [{ name: "PDF", extensions: ["pdf"] }],
  });
  if (typeof sel === "string") loadPdf(sel);
}

// ---------- views ----------
function stepper(): string {
  const steps = [
    ["drop", "Drop book"],
    ["detect", "Detect chapters"],
    ["review", "Review & voice"],
    ["run", "Generate"],
    ["done", "Done"],
  ] as const;
  const order: Stage[] = ["drop", "detect", "review", "run", "done"];
  const cur = order.indexOf(state.stage);
  return `<div class="stepper">${steps
    .map(([, label], i) => {
      const cls = i < cur ? "done" : i === cur ? "active" : "";
      const arrow = i < steps.length - 1 ? `<span class="arrow">→</span>` : "";
      return `<span class="step ${cls}"><span class="num">${
        i < cur ? "✓" : i + 1
      }</span>${esc(label)}</span>${arrow}`;
    })
    .join("")}</div>`;
}

function header(): string {
  let ollama: string;
  if (!state.ollamaChecked) {
    ollama = `<span class="pill"><span class="spin"></span>Checking Ollama…</span>`;
  } else if (state.ollamaUp) {
    ollama = `<span class="pill"><span class="dot ok"></span>Ollama${
      state.model ? ` · ${esc(state.model.split(":")[0])}` : ""
    }</span>`;
  } else {
    ollama = `<span class="pill"><span class="dot bad"></span>Ollama offline</span>`;
  }
  return `
  <div class="header">
    <div class="brand">
      <div class="logo">A</div>
      <div>
        <h1>Audiobook Studio</h1>
        <div class="sub">Local LLM chaptering · Kokoro narration · $0</div>
      </div>
    </div>
    <div class="statuses">${ollama}</div>
  </div>`;
}

function banner(): string {
  if (!state.error) return "";
  return `<div class="banner error"><strong>Error.</strong>&nbsp;${esc(
    state.error
  )}</div>`;
}

function dropView(): string {
  // Loading state: inspecting/extracting the PDF (can take seconds on big books).
  if (state.busy) {
    return `
  <div class="card">
    <div class="dropzone loading" aria-busy="true">
      <div class="spin big-spin"></div>
      <div class="big">Reading ${esc(state.loadingFile ?? "PDF")}…</div>
      <div class="small">Extracting and analyzing pages. Large books can take a moment.</div>
    </div>
  </div>
  <div class="card">
    <div class="skeleton-row"><span class="sk sk-icon"></span><span class="sk sk-line"></span></div>
    <div class="sk sk-line" style="width:60%;margin-top:12px"></div>
    <div class="sk sk-line" style="width:40%;margin-top:8px"></div>
  </div>`;
  }
  return `
  <div class="card">
    <div id="dz" class="dropzone">
      <div class="big">Drop a book PDF here</div>
      <div class="small">or <u>browse</u> to choose a file. A local model finds the chapters; Kokoro narrates them.</div>
    </div>
  </div>`;
}

function detectView(): string {
  const b = state.book!;
  const modelOpts = state.models.length
    ? state.models
        .map(
          (m) =>
            `<option value="${esc(m)}" ${
              m === state.model ? "selected" : ""
            }>${esc(m)}</option>`
        )
        .join("")
    : `<option value="">No models found</option>`;
  const outline = b.outline.length
    ? `<span class="pill"><span class="dot ok"></span>${b.outline.length} outline entries</span>`
    : `<span class="pill"><span class="dot warn"></span>No embedded outline — scanning text</span>`;

  return `
  <div class="card">
    <div class="filerow">
      <div class="meta">
        <div class="ficon">PDF</div>
        <div>
          <div class="fname">${esc(b.file_name)}</div>
          <div class="fsub">${b.page_count} pages · ${b.size_mb.toFixed(1)} MB</div>
        </div>
      </div>
      <button class="btn ghost" id="change">Change</button>
    </div>
  </div>

  <div class="card">
    <h2>Find chapters</h2>
    <p class="hint">A local model reads the structure and proposes audiobook chapters. You can edit them next.</p>
    <div class="row">
      <div class="field">
        <label for="model">Local model (Ollama)</label>
        <select id="model" ${state.busy ? "disabled" : ""}>${modelOpts}</select>
      </div>
    </div>
    <div class="row" style="align-items:center;gap:12px">${outline}</div>
    <div class="actions">
      <button class="btn secondary" id="back">Back</button>
      <button class="btn primary" id="detect" ${
        state.busy || !state.model ? "disabled" : ""
      }>${state.busy ? `<span class="spin"></span> Detecting…` : "Detect Chapters"}</button>
    </div>
    ${state.busy && state.progress ? progressBlock(false) : ""}
  </div>`;
}

function reviewView(): string {
  const rows = state.chapters
    .map(
      (c, i) => `
    <tr data-i="${i}">
      <td class="idx">${String(c.order).padStart(2, "0")}</td>
      <td><input type="text" class="ch-title" data-i="${i}" value="${esc(
        c.title
      )}" /></td>
      <td class="pages">${c.start_page}–${c.end_page}</td>
    </tr>`
    )
    .join("");

  const voiceOpts = VOICES.map(
    (v) =>
      `<option value="${v.id}" data-lang="${v.lang}" ${
        v.id === state.voice.voice ? "selected" : ""
      }>${esc(v.label)}</option>`
  ).join("");

  return `
  <div class="card">
    <h2>Chapters</h2>
    <p class="hint">${state.chapters.length} chapters detected. Rename any title; page ranges are derived from the boundaries.</p>
    <table class="chapters">
      <thead><tr><th>#</th><th>Title</th><th>Pages</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
  </div>

  <div class="card">
    <h2>Narration</h2>
    <p class="hint">Kokoro runs locally. British George at 1× matches the original build.</p>
    <div class="row">
      <div class="field">
        <label for="voice">Voice</label>
        <select id="voice">${voiceOpts}</select>
      </div>
      <div class="field">
        <label for="speed">Speed · <span id="speedval">${state.voice.speed.toFixed(
          2
        )}×</span></label>
        <input type="range" id="speed" min="0.7" max="1.3" step="0.05" value="${
          state.voice.speed
        }" />
      </div>
    </div>
    <div class="row">
      <div class="field">
        <label for="title">Title</label>
        <input type="text" id="title" value="${esc(state.title)}" />
      </div>
      <div class="field">
        <label for="author">Author</label>
        <input type="text" id="author" value="${esc(
          state.author
        )}" placeholder="e.g. William Lane Craig" />
      </div>
    </div>
    <div class="field">
      <label for="outdir">Output folder</label>
      <input type="text" id="outdir" value="${esc(state.outDir)}" />
    </div>
    <label class="check-row" for="polish">
      <input type="checkbox" id="polish" ${
        state.ollamaUp && state.polish ? "checked" : ""
      } ${state.ollamaUp ? "" : "disabled"} />
      <span>
        <span class="check-title">Polish transcripts with the local model${
          state.ollamaUp ? " (recommended)" : ""
        }</span>
        <span class="check-sub">${
          state.ollamaUp
            ? `Uses ${
                state.model ? esc(state.model) : "the selected model"
              } to remove front-matter, cover/title boilerplate, stray headings, and other artifacts the rules can't catch — these differ from book to book. Deletion only: your text is never rewritten, and each section falls back to the raw transcript if the model is unsure. Adds some time per chapter; untick to skip.`
            : "Requires Ollama to be running. Without it, transcripts use the built-in cleaner only."
        }</span>
      </span>
    </label>
    <div class="actions">
      <button class="btn secondary" id="back">Back</button>
      <button class="btn primary" id="generate">Generate Audiobook</button>
    </div>
  </div>`;
}

function progressBlock(showLog: boolean): string {
  const p = state.progress;
  const pct = p ? Math.round(p.pct) : 0;
  const meta = p
    ? `<span class="stage">${esc(p.stage)}</span><span>${esc(p.message)}${
        p.total > 1 ? ` · ${p.current}/${p.total}` : ""
      }</span>`
    : `<span class="stage">Starting…</span><span></span>`;
  const log = showLog
    ? `<div class="log" id="log">${state.log
        .map(
          (l) =>
            `<div class="line"><span class="t">${esc(l.t)}</span>  ${esc(
              l.msg
            )}</div>`
        )
        .join("")}</div>`
    : "";
  return `
    <div class="progress-wrap">
      <div class="bar"><span style="width:${pct}%"></span></div>
      <div class="progress-meta">${meta}</div>
      ${log}
    </div>`;
}

function runView(): string {
  return `
  <div class="card">
    <h2>Generating…</h2>
    <p class="hint">Transcribing pages, narrating each chapter with Kokoro, then bundling a chaptered .m4b. Long books take a while — it’s resumable if interrupted.</p>
    ${progressBlock(true)}
  </div>`;
}

function doneView(): string {
  const cover = state.book
    ? `style="background-image:url('cover.jpg')"`
    : "";
  return `
  <div class="card">
    <div class="result">
      <div class="cover" ${cover}></div>
      <div>
        <div class="ok-badge"><span class="dot ok"></span>Audiobook ready</div>
        <h2>${esc(state.title)}</h2>
        <p class="hint">${state.chapters.length} chapters · ${esc(
    VOICES.find((v) => v.id === state.voice.voice)?.label ?? state.voice.voice
  )} · ${state.voice.speed.toFixed(2)}×</p>
        <div class="actions" style="justify-content:flex-start;margin-top:12px">
          <button class="btn primary" id="reveal">Show in Finder</button>
          <button class="btn secondary" id="another">Make Another</button>
        </div>
      </div>
    </div>
  </div>`;
}

function body(): string {
  switch (state.stage) {
    case "drop":
      return dropView();
    case "detect":
      return detectView();
    case "review":
      return reviewView();
    case "run":
      return runView();
    case "done":
      return doneView();
  }
}

// ---------- render + events ----------
function render() {
  app.innerHTML = `<div class="app">${header()}${stepper()}${banner()}${body()}</div>`;
  bind();
}

function bind() {
  const dz = document.querySelector<HTMLDivElement>("#dz");
  if (dz) dz.onclick = browse;

  document.querySelector("#change")?.addEventListener("click", reset);
  document.querySelector("#back")?.addEventListener("click", () => {
    state.stage = state.stage === "review" ? "detect" : "drop";
    state.error = null;
    render();
  });
  document.querySelector("#detect")?.addEventListener("click", detect);
  document.querySelector("#generate")?.addEventListener("click", generate);
  document.querySelector("#another")?.addEventListener("click", reset);
  document.querySelector("#reveal")?.addEventListener("click", () => {
    if (state.resultPath) api.reveal(state.resultPath);
  });

  const model = document.querySelector<HTMLSelectElement>("#model");
  if (model) model.onchange = () => (state.model = model.value);

  document.querySelectorAll<HTMLInputElement>(".ch-title").forEach((inp) => {
    inp.onchange = () => {
      const i = Number(inp.dataset.i);
      state.chapters[i].title = inp.value;
    };
  });

  const voice = document.querySelector<HTMLSelectElement>("#voice");
  if (voice)
    voice.onchange = () => {
      state.voice.voice = voice.value;
      const lang = voice.selectedOptions[0]?.dataset.lang;
      if (lang) state.voice.lang = lang;
    };

  const speed = document.querySelector<HTMLInputElement>("#speed");
  if (speed)
    speed.oninput = () => {
      state.voice.speed = Number(speed.value);
      const lbl = document.querySelector("#speedval");
      if (lbl) lbl.textContent = `${state.voice.speed.toFixed(2)}×`;
    };

  const title = document.querySelector<HTMLInputElement>("#title");
  if (title) title.onchange = () => (state.title = title.value);
  const author = document.querySelector<HTMLInputElement>("#author");
  if (author) author.onchange = () => (state.author = author.value);
  const outdir = document.querySelector<HTMLInputElement>("#outdir");
  if (outdir) outdir.onchange = () => (state.outDir = outdir.value);
  const polish = document.querySelector<HTMLInputElement>("#polish");
  if (polish) polish.onchange = () => (state.polish = polish.checked);
}

// ---------- wire backend events + native drag-drop ----------
async function init() {
  await onProgress((p: Progress) => {
    state.progress = p;
    logLine(`[${p.stage}] ${p.message}${p.total > 1 ? ` (${p.current}/${p.total})` : ""}`);
    // Live-update only the dynamic regions to avoid losing focus on inputs.
    if (state.stage === "run") {
      const wrap = document.querySelector(".progress-wrap");
      if (wrap) {
        wrap.outerHTML = progressBlock(true).trim();
        const logEl = document.querySelector("#log");
        if (logEl) logEl.scrollTop = logEl.scrollHeight;
      } else {
        render();
      }
    } else if (state.busy) {
      render();
    }
  });

  // Native OS file drop (Tauri webview).
  await getCurrentWebview().onDragDropEvent((event) => {
    const dz = document.querySelector("#dz");
    if (event.payload.type === "over") {
      dz?.classList.add("drag");
    } else if (event.payload.type === "drop") {
      dz?.classList.remove("drag");
      const f = event.payload.paths?.[0];
      if (f && state.stage === "drop" && !state.busy) loadPdf(f);
    } else {
      dz?.classList.remove("drag");
    }
  });

  await refreshModels();
  render();
  // Poll Ollama status quietly.
  setInterval(async () => {
    const was = state.ollamaUp;
    await refreshModels();
    if (was !== state.ollamaUp && (state.stage === "drop" || state.stage === "detect")) {
      render();
    }
  }, 8000);
}

init();
