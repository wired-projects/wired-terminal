import { useCallback, useEffect, useState } from 'react';
import { api, desktop, type Check, type DiagnosticsReport } from '../lib/api';
import { briefly } from '../lib/errors';

/**
 * `hermes doctor`, with buttons.
 *
 * Every row is one link in the chain between a person and a working assistant,
 * and the broken one says which it is and what to press. **Copy diagnostics** is
 * the block whoever helps him is going to ask for, so it exists before the first
 * non-coder installs this rather than after the first phone call.
 */

interface DiagnosticsViewProps {
  onFix: (remedy: string) => void;
}

export function DiagnosticsView({ onFix }: DiagnosticsViewProps) {
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [confirmErase, setConfirmErase] = useState(false);

  const load = useCallback(async () => {
    try {
      setReport(await api.diagnostics());
      setError(null);
    } catch (err) {
      setError(briefly(err));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const copy = useCallback(async () => {
    if (!report) return;
    // Strip the log: it is the largest part and the least likely to be wanted
    // in a chat message. The Open log button is right there for the rest.
    const { recent_log, ...summary } = report;
    await navigator.clipboard.writeText(JSON.stringify(summary, null, 2));
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2500);
  }, [report]);

  if (!report) return <div className="screen-empty">{error ?? 'Checking…'}</div>;

  return (
    <div className="screen">
      <div className="screen-inner">
        <h2 className="screen-title">Help</h2>

        <section className="card">
          <h3>How things look</h3>
          <div className="check-list">
            {report.checks.map((check) => (
              <CheckRow key={check.id} check={check} onFix={onFix} />
            ))}
          </div>
          <div className="button-row">
            <button type="button" className="btn btn-secondary" onClick={() => void load()}>
              Check again
            </button>
          </div>
        </section>

        <section className="card">
          <h3>If you need to ask someone for help</h3>
          <p className="hint">
            Wired {report.version} · {report.os} {report.arch} · port {report.port}
          </p>
          <div className="button-row">
            <button type="button" className="btn btn-primary" onClick={() => void copy()}>
              {copied ? 'Copied' : 'Copy diagnostics'}
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => void desktop.openPath(report.log_file)}
            >
              Open logs folder
            </button>
          </div>
          <details>
            <summary>Show the last few log lines</summary>
            <pre className="log-dump">{report.recent_log.join('\n')}</pre>
          </details>
        </section>

        <section className="card">
          <h3>Starting over</h3>
          <p>
            This removes Wired's own settings, its history and its saved passwords. It does not
            touch your files, your folder, or the assistant you installed.
          </p>
          {confirmErase ? (
            <div className="button-row">
              <button
                type="button"
                className="btn btn-danger"
                onClick={() =>
                  void api
                    .eraseEverything()
                    .then(() => window.location.reload())
                    .catch((err) => setError(briefly(err)))
                }
              >
                Yes, erase it
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => setConfirmErase(false)}
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => setConfirmErase(true)}
            >
              Erase Wired's settings and history
            </button>
          )}
          {error && <p className="hint">{error}</p>}
        </section>
      </div>
    </div>
  );
}

const FIX_LABEL: Record<string, string> = {
  install: 'Set it up',
  login: 'Sign in',
  folder: 'Choose a folder',
  chat: 'Open Telegram settings',
};

function CheckRow({ check, onFix }: { check: Check; onFix: (remedy: string) => void }) {
  const state = check.ok === true ? 'good' : check.ok === false ? 'bad' : 'unknown';
  return (
    <div className="check-row" data-state={state}>
      <span className="check-mark" aria-hidden="true">
        {state === 'good' ? '✓' : state === 'bad' ? '!' : '?'}
      </span>
      <div className="check-body">
        <span className="check-label">{check.label}</span>
        <span className="hint">{check.detail}</span>
      </div>
      {check.fix && (
        <button type="button" className="btn btn-secondary" onClick={() => onFix(check.fix!)}>
          {FIX_LABEL[check.fix] ?? 'Fix'}
        </button>
      )}
    </div>
  );
}
