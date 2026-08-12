/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Backend origin, e.g. http://127.0.0.1:8000. Defaults to loopback:8000. */
  readonly VITE_API_BASE?: string;
  /** Must match WIRED_AUTH_TOKEN when the backend runs with auth enabled. */
  readonly VITE_AUTH_TOKEN?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
