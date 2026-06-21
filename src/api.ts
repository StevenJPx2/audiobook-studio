// Typed bridge to the Rust backend.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface OutlineItem {
  level: number;
  title: string;
  page: number;
}

export interface BookInfo {
  path: string;
  file_name: string;
  page_count: number;
  size_mb: number;
  outline: OutlineItem[];
}

export interface Chapter {
  order: number;
  title: string;
  start_page: number;
  end_page: number;
}

export interface VoiceConfig {
  voice: string;
  lang: string;
  speed: number;
}

export interface GenerateRequest {
  pdf_path: string;
  out_dir: string;
  chapters: Chapter[];
  voice: VoiceConfig;
  book_title: string;
  author: string;
}

export interface Progress {
  stage: string;
  message: string;
  current: number;
  total: number;
  pct: number;
}

export const api = {
  inspectPdf: (path: string) => invoke<BookInfo>("inspect_pdf", { path }),
  listModels: () => invoke<string[]>("list_models"),
  detectChapters: (path: string, model: string) =>
    invoke<Chapter[]>("detect_chapters", { path, model }),
  generate: (req: GenerateRequest) =>
    invoke<string>("generate_audiobook", { req }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  defaultVoice: () => invoke<VoiceConfig>("default_voice"),
};

export function onProgress(cb: (p: Progress) => void): Promise<UnlistenFn> {
  return listen<Progress>("audiobook://progress", (e) => cb(e.payload));
}
