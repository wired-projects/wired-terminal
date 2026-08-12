import { useCallback, useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { api, websocketUrl, type ProviderId } from '../lib/api';
import { FONT_MONO, xtermTheme } from '../lib/theme';

function base64ToBytes(chunk: string): Uint8Array {
  const binary = atob(chunk);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

export interface TerminalReadyInfo {
  running: boolean;
  provider: string | null;
  generation: number | null;
}

export interface TerminalPanelProps {
  provider: ProviderId;
  /** When true, POST /api/pty/start once the WebSocket is open. */
  shouldStart: boolean;
  colsHint?: number;
  rowsHint?: number;
  onExit: () => void;
  onReady: (info: TerminalReadyInfo) => void;
  onError: (message: string) => void;
}

export function TerminalPanel({
  provider,
  shouldStart,
  colsHint = 80,
  rowsHint = 24,
  onExit,
  onReady,
  onError,
}: TerminalPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [connecting, setConnecting] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Stable refs so the terminal effect does not tear down on parent re-renders.
  const onExitRef = useRef(onExit);
  const onReadyRef = useRef(onReady);
  const onErrorRef = useRef(onError);
  onExitRef.current = onExit;
  onReadyRef.current = onReady;
  onErrorRef.current = onError;

  const postResize = useCallback((cols: number, rows: number) => {
    // The session may not be up yet; a failed resize is not worth surfacing.
    void api.resizePty(cols, rows).catch(() => {});
  }, []);

  useEffect(() => {
    // Captured once for this effect run. The parent clears shouldStart after
    // onReady, and that must not re-run the effect (see the deps below).
    const startOnOpen = shouldStart;
    let disposed = false;
    let fitTimer: ReturnType<typeof setTimeout> | null = null;

    setConnecting(true);
    setError(null);

    const term = new Terminal({
      fontSize: 13,
      fontFamily: FONT_MONO,
      cursorBlink: true,
      cursorStyle: 'bar',
      cursorWidth: 2,
      scrollback: 5000,
      allowProposedApi: true,
      theme: xtermTheme,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    termRef.current = term;
    fitAddonRef.current = fitAddon;

    const host = containerRef.current;
    if (!host) {
      return () => term.dispose();
    }

    term.open(host);
    try {
      fitAddon.fit();
    } catch {
      // First fit can race the initial layout; the observer below retries.
    }

    const fitAndNotify = () => {
      if (disposed || !termRef.current || !fitAddonRef.current) return;
      // A hidden panel (the transcript tab is showing) measures zero. Fitting
      // to that would push a 1x1 geometry to the PTY and wreck the TUI's
      // layout for the session, so wait until the panel is on screen again.
      if (!host.clientWidth || !host.clientHeight) return;
      try {
        fitAddonRef.current.fit();
        const { cols, rows } = termRef.current;
        postResize(cols, rows);
      } catch {
        // Container can still be mid-layout on the first pass.
      }
    };

    const socket = new WebSocket(websocketUrl());

    socket.onopen = () => {
      if (disposed) return;
      setConnecting(false);
      term.focus();

      fitTimer = setTimeout(async () => {
        if (disposed) return;
        fitAndNotify();
        const cols = termRef.current?.cols ?? colsHint;
        const rows = termRef.current?.rows ?? rowsHint;

        if (!startOnOpen) {
          // Attaching to an existing session — just match its geometry.
          postResize(cols, rows);
          onReadyRef.current({ running: true, provider, generation: null });
          return;
        }

        try {
          const data = await api.startPty(provider, cols, rows);
          if (disposed) return;
          onReadyRef.current({
            running: true,
            provider: data.provider ?? provider,
            generation: data.generation ?? null,
          });
          fitAndNotify();
        } catch (e) {
          if (disposed) return;
          const message = (e as Error).message || 'Failed to start PTY';
          setError(message);
          onErrorRef.current(message);
        }
      }, 40);
    };

    socket.onmessage = (event) => {
      if (disposed) return;
      try {
        const msg = JSON.parse(event.data as string);
        if (msg.type === 'init') {
          onReadyRef.current({
            running: !!msg.running,
            provider: msg.provider ?? null,
            generation: msg.generation ?? null,
          });
        } else if (msg.type === 'data') {
          term.write(base64ToBytes(msg.chunk));
        } else if (msg.type === 'exit') {
          term.writeln('\r\n\x1b[90m[session ended]\x1b[0m');
          onExitRef.current();
        }
      } catch {
        term.write(String(event.data));
      }
    };

    socket.onerror = () => {
      if (disposed) return;
      const message =
        'Could not connect to the terminal backend. Start it with `npm run start` (port 8000).';
      setError(message);
      setConnecting(false);
      onErrorRef.current(message);
    };

    socket.onclose = () => {
      if (!disposed) setConnecting(false);
    };

    const dataDisposable = term.onData((data) => {
      if (socket.readyState === WebSocket.OPEN) socket.send(data);
    });

    const resizeObserver = new ResizeObserver(() => {
      if (fitTimer) clearTimeout(fitTimer);
      fitTimer = setTimeout(fitAndNotify, 50);
    });
    resizeObserver.observe(host);

    const onWindowResize = () => fitAndNotify();
    window.addEventListener('resize', onWindowResize);

    return () => {
      disposed = true;
      window.removeEventListener('resize', onWindowResize);
      if (fitTimer) clearTimeout(fitTimer);
      dataDisposable.dispose();
      resizeObserver.disconnect();
      socket.close();
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
    // shouldStart is intentionally omitted: it is only the mount-time intent to
    // spawn a PTY. The parent clears it after onReady, which must not tear down
    // the live terminal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider, colsHint, rowsHint, postResize]);

  return (
    <div className="panel-fill fade-in">
      {connecting && !error && (
        <div className="overlay overlay-connecting">
          <span className="spinner" />
          Connecting…
        </div>
      )}

      {error && (
        <div className="overlay overlay-error" role="alert">
          <div className="overlay-icon" aria-hidden="true">
            !
          </div>
          <div className="overlay-title">Connection error</div>
          <div className="overlay-body">{error}</div>
        </div>
      )}

      <div ref={containerRef} className="term-host" />
    </div>
  );
}
