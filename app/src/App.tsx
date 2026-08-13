import { useCallback, useEffect, useRef, useState } from 'react';
import { WiredWordmark } from './components/Brand';
import { Composer } from './components/Composer';
import { DiagnosticsView } from './components/DiagnosticsView';
import { HistoryView } from './components/HistoryView';
import { ScheduleView } from './components/ScheduleView';
import { SettingsView } from './components/SettingsView';
import { StatusPill } from './components/StatusPill';
import { TerminalPanel, type TerminalReadyInfo } from './components/TerminalPanel';
import { TranscriptPanel } from './components/TranscriptPanel';
import { Welcome } from './components/Welcome';
import { Wizard } from './components/Wizard';
import { providerMeta } from './components/ProviderIcon';
import { useBackendStatus } from './hooks/useBackendStatus';
import { api, desktop, isDesktopApp, type ProviderId, type UpdateStatus } from './lib/api';
import { explain, type Remedy } from './lib/errors';

/**
 * The app leads with the conversation.
 *
 * It used to open on a raw full-screen TUI repainting itself, with the message
 * box in a left sidebar headed "Command your agent" and the terminal's own
 * keystroke controls next to it. The transcript was always the humane view; it
 * is now the default, and everything that pokes a PTY lives behind Terminal.
 */

type View = 'chat' | 'terminal' | 'history' | 'schedule' | 'settings' | 'help';

const TABS: { id: View; label: string }[] = [
  { id: 'chat', label: 'Chat' },
  { id: 'history', label: 'History' },
  { id: 'schedule', label: 'Schedule' },
  { id: 'settings', label: 'Settings' },
  { id: 'help', label: 'Help' },
];

/** How long after the last transcript row we still call it "working". */
const BUSY_MS = 3500;

export default function App() {
  const {
    online,
    health,
    available,
    keepAlive,
    session,
    setSession,
    preferredProvider,
    probe,
    sync,
  } = useBackendStatus();

  const [view, setView] = useState<View>('chat');
  const [banner, setBanner] = useState<ReturnType<typeof explain> | null>(null);
  const [starting, setStarting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [prefill, setPrefill] = useState<string | undefined>();
  const [skippedSetup, setSkippedSetup] = useState(false);
  // Remounts the panels so a new session never inherits the old one's buffer.
  const [sessionKey, setSessionKey] = useState(0);

  const provider: ProviderId = session.provider ?? preferredProvider;
  const active = providerMeta(provider);

  // ── activity ─────────────────────────────────────────────────────────
  const busyTimer = useRef<number | undefined>();
  const markBusy = useCallback(() => {
    setBusy(true);
    window.clearTimeout(busyTimer.current);
    busyTimer.current = window.setTimeout(() => setBusy(false), BUSY_MS);
  }, []);
  useEffect(() => () => window.clearTimeout(busyTimer.current), []);

  // ── one place errors become something to press ────────────────────────
  const fail = useCallback((error: unknown) => setBanner(explain(error)), []);

  const startSession = useCallback(
    async (selected: ProviderId = provider) => {
      setBanner(null);
      setStarting(true);
      try {
        if (!(await probe())) {
          setBanner(explain('offline'));
          return;
        }
        await api.startAgent(selected, keepAlive);
        setSessionKey((k) => k + 1);
        setSession({ running: true, provider: selected, generation: null });
      } catch (err) {
        fail(err);
      } finally {
        setStarting(false);
      }
    },
    [provider, probe, keepAlive, setSession, fail],
  );

  const stopSession = useCallback(async () => {
    try {
      await api.stopAgent();
    } catch {
      // Fall back to the low-level kill if the supervisor call fails.
      await api.killPty().catch(() => {});
    }
    setSession({ running: false, provider: null, generation: null });
  }, [setSession]);

  const applyRemedy = useCallback(
    (remedy: Remedy | string) => {
      setBanner(null);
      switch (remedy) {
        case 'retry':
          void sync();
          break;
        case 'restart-assistant':
          void startSession();
          break;
        case 'install':
        case 'login':
        case 'open-setup':
          setSkippedSetup(false);
          setView('chat');
          break;
        case 'folder':
        case 'open-settings':
        case 'chat':
          setView('settings');
          break;
        case 'open-logs':
        case 'open-help':
          setView('help');
          break;
      }
    },
    [sync, startSession],
  );

  const send = useCallback(
    async (text: string) => {
      try {
        await api.sendMessage(text, true);
        markBusy();
        void sync();
        return true;
      } catch (err) {
        fail(err);
        return false;
      }
    },
    [sync, fail, markBusy],
  );

  const answerPrompt = useCallback(
    async (allow: boolean) => {
      try {
        await api.approve(allow);
        markBusy();
        return true;
      } catch (err) {
        fail(err);
        return false;
      }
    },
    [fail, markBusy],
  );

  // ── is a newer version out ───────────────────────────────────────────
  // Asked once, on launch. There is no auto-update to trigger: the artefacts
  // are not signed yet, so the honest move is to say a version exists and let
  // the person decide. A failed check is silent — being offline is not an
  // error worth a banner.
  const [update, setUpdate] = useState<UpdateStatus | null>(null);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  useEffect(() => {
    if (online !== true) return;
    let cancelled = false;
    void api
      .update()
      .then((status) => {
        if (!cancelled && status.available) setUpdate(status);
      })
      .catch(() => {
        /* no version banner is better than a wrong one */
      });
    return () => {
      cancelled = true;
    };
  }, [online]);

  // Installing replaces this app and restarts it, so a success never comes
  // back here — only a failure does, and that has to be said rather than
  // leaving the button spinning forever.
  const [updating, setUpdating] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const installUpdate = useCallback(async () => {
    setUpdating(true);
    setUpdateError(null);
    try {
      await desktop.installUpdate();
      // Reached only if the restart did not happen.
      setUpdateError('The update installed but the app did not restart — quit and reopen it.');
    } catch (err) {
      setUpdateError(err instanceof Error ? err.message : String(err));
    } finally {
      setUpdating(false);
    }
  }, []);

  // ── notifications ────────────────────────────────────────────────────
  const notifiedFor = useRef<number | null>(null);
  useEffect(() => {
    const pending = health?.pending_prompt;
    if (!pending || health?.security == null) return;
    if (notifiedFor.current === pending.seq) return;
    notifiedFor.current = pending.seq;
    if (document.hasFocus() && view === 'chat') return;
    void desktop.notify('Your assistant needs an answer', pending.text.slice(0, 180));
  }, [health?.pending_prompt, health?.security, view]);

  const handleReady = useCallback(
    (info: TerminalReadyInfo) => {
      setSession((prev) => ({
        running: info.running || prev.running,
        provider: (info.provider as ProviderId) ?? prev.provider,
        generation: info.generation ?? prev.generation,
      }));
    },
    [setSession],
  );

  // keep-alive may bring the session straight back, so re-sync instead of clearing.
  const handleExit = useCallback(() => void sync(), [sync]);

  const askAgain = useCallback((text: string) => {
    setPrefill(text);
    setView('chat');
  }, []);

  // ── plain status, not `CLAUDE · 24/7 · gen 3` ────────────────────────
  const status = (() => {
    if (online === false) return { label: "Wired isn't running", tone: false as const };
    if (health?.pending_prompt) return { label: 'Waiting for you', tone: null };
    if (!session.running) return { label: 'Not running', tone: null };
    if (busy) return { label: 'Working on it…', tone: true as const };
    return { label: 'Ready', tone: true as const };
  })();

  const needsSetup = health != null && !health.onboarded && !skippedSetup;

  if (needsSetup) {
    return (
      <div className="app">
        <header className="header">
          <WiredWordmark />
        </header>
        <div className="app-body">
          <main className="app-main">
            <Wizard
              onFinished={() => {
                void sync();
                setSkippedSetup(false);
                void startSession();
              }}
              onSkip={() => setSkippedSetup(true)}
            />
          </main>
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      <header className="header">
        <WiredWordmark />

        <nav className="tabs" aria-label="Views">
          {TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              aria-current={view === tab.id || (tab.id === 'chat' && view === 'terminal')}
              onClick={() => setView(tab.id)}
            >
              {tab.label}
              {tab.id === 'settings' && (health?.chat.pending_pairings ?? 0) > 0 && (
                <span className="tab-badge" aria-label="waiting to be approved" />
              )}
            </button>
          ))}
        </nav>

        <div className="header-status">
          <StatusPill state={status.tone} label={status.label} />
          {session.running ? (
            <button type="button" className="btn btn-danger" onClick={() => void stopSession()}>
              Stop everything
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-primary"
              disabled={starting || online === false || available[provider] === false}
              onClick={() => void startSession()}
            >
              {starting ? 'Starting…' : 'Start'}
            </button>
          )}
        </div>
      </header>

      {update && !updateDismissed && (
        <div className="banner banner-info fade-in" role="status">
          <span>
            <strong>Version {update.latest} is out.</strong> This is {update.current}.
          </span>
          {updateError && <span className="banner-note">{updateError}</span>}
          <span className="button-row">
            {isDesktopApp() ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={updating}
                onClick={() => void installUpdate()}
              >
                {updating ? 'Installing…' : 'Install and restart'}
              </button>
            ) : (
              update.download && (
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => void desktop.openPath(update.download!)}
                >
                  Download it
                </button>
              )
            )}
            <button
              type="button"
              className="btn btn-ghost"
              disabled={updating}
              onClick={() => setUpdateDismissed(true)}
            >
              Later
            </button>
          </span>
        </div>
      )}

      {banner && (
        <div className="banner fade-in" role="alert">
          <span>
            <strong>{banner.title}.</strong> {banner.detail}
          </span>
          <span className="button-row">
            {banner.action && (
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => applyRemedy(banner.action!.remedy)}
              >
                {banner.action.label}
              </button>
            )}
            <button type="button" className="btn btn-ghost" onClick={() => setBanner(null)}>
              Dismiss
            </button>
          </span>
        </div>
      )}

      <div className="app-body">
        <main className="app-main">
          {(view === 'chat' || view === 'terminal') && (
            <>
              <div className="toolbar">
                <div className="toolbar-meta">
                  <span className="toolbar-title">{active?.label ?? 'Your assistant'}</span>
                  <span className="toolbar-note">
                    {keepAlive ? 'always on' : 'stops when it exits'}
                  </span>
                </div>
                <div className="segmented" role="tablist" aria-label="Conversation view">
                  {(['chat', 'terminal'] as const).map((id) => (
                    <button
                      key={id}
                      type="button"
                      role="tab"
                      aria-selected={view === id}
                      onClick={() => setView(id)}
                    >
                      {id === 'chat' ? 'Conversation' : 'Terminal'}
                    </button>
                  ))}
                </div>
              </div>

              {/*
                Both panels stay mounted so switching views does not drop the
                WebSocket or replay the whole transcript. `.panel-layer` hides
                the inactive one without collapsing its box — see styles.css.
              */}
              <div className="panel-stack">
                <div
                  className="panel-layer"
                  data-active={view === 'chat'}
                  aria-hidden={view !== 'chat'}
                >
                  <TranscriptPanel
                    sessionKey={sessionKey}
                    onAnswer={answerPrompt}
                    onActivity={markBusy}
                    empty={
                      <Welcome
                        assistantName={active?.label ?? 'your assistant'}
                        onStart={(text) => setPrefill(text)}
                      />
                    }
                  />
                  <Composer
                    onSend={send}
                    disabled={online === false}
                    folder={health?.folder}
                    prefill={prefill}
                  />
                </div>
                <div
                  className="panel-layer"
                  data-active={view === 'terminal'}
                  aria-hidden={view !== 'terminal'}
                >
                  <TerminalPanel
                    key={`term-${provider}-${sessionKey}`}
                    provider={provider}
                    shouldStart={false}
                    onExit={handleExit}
                    onReady={handleReady}
                    onError={(message) => fail(message)}
                  />
                </div>
              </div>
            </>
          )}

          {view === 'history' && <HistoryView onAskAgain={askAgain} />}
          {view === 'schedule' && <ScheduleView />}
          {view === 'settings' && (
            <SettingsView
              onChanged={() => void sync()}
              onRestartAssistant={(next) => void startSession(next)}
            />
          )}
          {view === 'help' && <DiagnosticsView onFix={applyRemedy} />}
        </main>
      </div>
    </div>
  );
}
