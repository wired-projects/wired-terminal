import { useCallback, useEffect, useRef, useState } from 'react';
import { WiredAppIcon } from './Brand';
import { ProviderIcon } from './ProviderIcon';
import { TerminalPanel } from './TerminalPanel';
import { api, desktop, type ProviderId, type SetupState } from '../lib/api';
import { briefly } from '../lib/errors';

/**
 * First run, with no commands typed.
 *
 * The old unconfigured state was two greyed-out cards reading "Not installed" —
 * a dead end inside a working app. Each step here ends in a button rather than
 * an instruction, including the sign-in, which happens in a terminal this app
 * already owns rather than one the user has to find.
 */

type Step = 'intro' | 'install' | 'folder' | 'signin' | 'done';

const ORDER: Step[] = ['intro', 'install', 'folder', 'signin', 'done'];

const TITLES: Record<Step, string> = {
  intro: 'What this is',
  install: 'Install your assistant',
  folder: 'Choose a folder',
  signin: 'Sign in',
  done: 'Ready',
};

const POLL_MS = 1500;

interface WizardProps {
  onFinished: () => void;
  /** Lets the user get out of the way and use the app as-is. */
  onSkip: () => void;
}

export function Wizard({ onFinished, onSkip }: WizardProps) {
  const [step, setStep] = useState<Step>('intro');
  const [state, setState] = useState<SetupState | null>(null);
  const [provider, setProvider] = useState<ProviderId>('claude');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [signinStarted, setSigninStarted] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const next = await api.setupState();
      if (!mounted.current) return next;
      setState(next);
      return next;
    } catch (err) {
      if (mounted.current) setError(briefly(err));
      return null;
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Only poll while something is actually moving.
  const installing = state?.install.status === 'running';
  useEffect(() => {
    if (!installing && step !== 'signin') return;
    const id = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(id);
  }, [installing, step, refresh]);

  const run = useCallback(async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      return true;
    } catch (err) {
      setError(briefly(err));
      return false;
    } finally {
      setBusy(false);
    }
  }, []);

  const chosen = state?.providers.find((p) => p.id === provider);
  const installed = chosen?.available ?? false;

  const finish = useCallback(async () => {
    await run(() => api.saveSettings({ onboarded: true, assistant: provider }));
    onFinished();
  }, [run, onFinished, provider]);

  return (
    <div className="wizard fade-in">
      <ol className="wizard-steps" aria-label="Setup progress">
        {ORDER.map((id, index) => (
          <li
            key={id}
            data-state={
              id === step ? 'current' : ORDER.indexOf(step) > index ? 'done' : 'todo'
            }
          >
            <span className="dot" aria-hidden="true">
              {ORDER.indexOf(step) > index ? '✓' : index + 1}
            </span>
            {TITLES[id]}
          </li>
        ))}
      </ol>

      <div className="wizard-body">
        {error && (
          <div className="banner" role="alert">
            <span>{error}</span>
            <button type="button" className="btn btn-ghost" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        )}

        {step === 'intro' && (
          <section className="wizard-panel">
            <WiredAppIcon size={48} />
            <h2>A helper that keeps working when you close the window</h2>
            <p>
              Wired runs an AI assistant on this computer. You ask it for things in plain English,
              and it can read and change files in <strong>one folder that you choose</strong>.
            </p>
            <ul className="plain-list">
              <li>
                <strong>What it can touch.</strong> Only the folder you pick in a moment. Nothing
                else on the computer, unless you change that later.
              </li>
              <li>
                <strong>What "always on" means.</strong> It keeps running in the background so it
                can answer you from your phone and do things on a schedule. There is a Wired icon in
                your menu bar the whole time.
              </li>
              <li>
                <strong>How to stop it.</strong> Click that icon and choose <em>Stop the
                assistant</em>. It stops immediately.
              </li>
              <li>
                <strong>It acts on its own.</strong> Inside the folder you pick, it does not
                stop to ask. Stop it from the menu bar icon if you need to.
              </li>
            </ul>
            <div className="wizard-actions">
              <button type="button" className="btn btn-primary btn-lg" onClick={() => setStep('install')}>
                Get started
              </button>
              <button type="button" className="btn btn-ghost" onClick={onSkip}>
                Skip setup
              </button>
            </div>
          </section>
        )}

        {step === 'install' && state && (
          <section className="wizard-panel">
            <h2>Which assistant would you like?</h2>
            <div className="provider-grid">
              {state.providers.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className="provider-card"
                  aria-pressed={provider === p.id}
                  data-selected={provider === p.id}
                  onClick={() => setProvider(p.id)}
                >
                  <span className="icon">
                    <ProviderIcon id={p.id} />
                  </span>
                  <span className="name">{p.label}</span>
                  <span className="blurb">{p.available ? 'Already installed' : p.detail}</span>
                </button>
              ))}
            </div>

            {!state.node.found && !installed && (
              <div className="notice-block" role="status">
                <p>
                  <strong>One thing first.</strong> Assistants are installed with a tool called
                  Node.js, and it isn't on this computer yet.
                </p>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => void desktop.openPath(state.node.download)}
                >
                  Open the Node.js download page
                </button>
                <p className="hint">
                  Install it, then come back here and press Check again. Nothing else is needed.
                </p>
                <button type="button" className="btn btn-ghost" onClick={() => void refresh()}>
                  Check again
                </button>
              </div>
            )}

            {state.install.status === 'running' && (
              <div className="progress-line" role="status" aria-live="polite">
                <span className="spinner" aria-hidden="true" />
                {state.install.message || 'Working…'}
              </div>
            )}
            {state.install.status === 'failed' && (
              <div className="notice-block notice-bad" role="alert">
                <p>{state.install.message}</p>
                <details>
                  <summary>Show the details</summary>
                  <pre className="log-dump">{state.install.log.join('\n')}</pre>
                </details>
              </div>
            )}

            <div className="wizard-actions">
              {installed ? (
                <button
                  type="button"
                  className="btn btn-primary btn-lg"
                  onClick={() => setStep('folder')}
                >
                  Next
                </button>
              ) : (
                <button
                  type="button"
                  className="btn btn-primary btn-lg"
                  disabled={busy || installing || !state.node.found}
                  onClick={() =>
                    void run(async () => {
                      await api.install(provider);
                      await refresh();
                    })
                  }
                >
                  {installing ? 'Installing…' : `Install ${chosen?.label ?? ''}`}
                </button>
              )}
              <button type="button" className="btn btn-ghost" onClick={() => setStep('intro')}>
                Back
              </button>
            </div>
          </section>
        )}

        {step === 'folder' && state && (
          <section className="wizard-panel">
            <h2>Which folder may your assistant read and write?</h2>
            <p>
              Everything it does happens inside this one folder. Anything outside it is off limits.
            </p>
            <FolderPicker
              suggested={state.folder.chosen ?? state.folder.suggested}
              busy={busy}
              onChoose={(folder) =>
                void run(async () => {
                  await api.setFolder(folder);
                  await refresh();
                  setStep('signin');
                })
              }
            />
            <div className="wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => setStep('install')}>
                Back
              </button>
            </div>
          </section>
        )}

        {step === 'signin' && (
          <section className="wizard-panel wizard-panel-wide">
            <h2>Sign in to {chosen?.label ?? 'your assistant'}</h2>
            <p>
              This part happens in your assistant's own window below. Press the button, then answer
              its questions — it will open your browser if it needs to.
            </p>

            {chosen?.signed_in && !signinStarted && (
              <div className="notice-block notice-good" role="status">
                It looks like you are already signed in. You can skip this step.
              </div>
            )}

            <div className="wizard-actions">
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await api.login(provider);
                    setSigninStarted(true);
                  })
                }
              >
                {signinStarted ? 'Start again' : 'Open the sign-in'}
              </button>
              <button type="button" className="btn btn-primary btn-lg" onClick={() => setStep('done')}>
                {chosen?.signed_in || signinStarted ? "I'm signed in" : 'Skip for now'}
              </button>
              <button type="button" className="btn btn-ghost" onClick={() => setStep('folder')}>
                Back
              </button>
            </div>

            {signinStarted && (
              <div className="wizard-terminal">
                <TerminalPanel
                  provider={provider}
                  shouldStart={false}
                  onExit={() => void refresh()}
                  onReady={() => {}}
                  onError={(message) => setError(message)}
                />
              </div>
            )}
          </section>
        )}

        {step === 'done' && (
          <section className="wizard-panel">
            <WiredAppIcon size={48} />
            <h2>That's everything</h2>
            <p>
              Your assistant is working in{' '}
              <code>{state?.folder.chosen ?? state?.folder.current}</code>. Ask it something, and it
              will keep going after you close this window.
            </p>
            <p className="hint">
              Want to talk to it from your phone? Settings → Telegram takes about a minute.
            </p>
            <div className="wizard-actions">
              <button
                type="button"
                className="btn btn-primary btn-lg"
                disabled={busy}
                onClick={() => void finish()}
              >
                Start using it
              </button>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

/**
 * Native picker where there is one, a text field where there is not. The
 * default is a folder we will create, not the whole home directory.
 */
export function FolderPicker({
  suggested,
  busy,
  onChoose,
  label = 'Use this folder',
}: {
  suggested: string;
  busy?: boolean;
  onChoose: (folder: string) => void;
  label?: string;
}) {
  const [folder, setFolder] = useState(suggested);

  useEffect(() => setFolder(suggested), [suggested]);

  return (
    <div className="folder-picker">
      <input
        className="field"
        value={folder}
        spellCheck={false}
        aria-label="Assistant folder"
        onChange={(e) => setFolder(e.target.value)}
      />
      <div className="button-row">
        <button
          type="button"
          className="btn btn-secondary"
          onClick={() =>
            void desktop.pickFolder(folder).then((picked) => {
              if (picked) setFolder(picked);
            })
          }
        >
          Browse…
        </button>
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !folder.trim()}
          onClick={() => onChoose(folder.trim())}
        >
          {label}
        </button>
      </div>
    </div>
  );
}
