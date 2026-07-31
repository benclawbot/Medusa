/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_MEDUSA_OPENAI_REALTIME_EVIDENCE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
