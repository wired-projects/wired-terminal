import { useCallback, useEffect, useState } from 'react';
import { FolderPicker } from './Wizard';
import { PROVIDERS } from './ProviderIcon';
import {
  api,
  desktop,
  type GatewayStatus,
  type ProviderId,
  type Settings,
} from '../lib/api';
import { briefly } from '../lib/errors';

/**
 * The things he will actually change — which assistant, the folder, always-on,
 * and start-at-login — plus the chat bridge, which is the reason the phone in
 * his pocket can reach any of it.
 *
 * Everything here was previously an environment variable in a file he has no
 * way to open. Where one of those variables is still set, the control says so
 * and stays read-only rather than pretending to work.
 */

interface SettingsViewProps {
  onChanged: () => void;
  /** Takes the assistant to come back up as — a switch restarts into the new one. */
  onRestartAssistant: (provider: ProviderId) => void;
}

export function SettingsView({ onChanged, onRestartAssistant }: SettingsViewProps) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [next, chat] = await Promise.all([api.settings(), api.gateway()]);
      setSettings(next);
      setGateway(chat);
    } catch (err) {
      setError(briefly(err));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Pairing codes expire in an hour and arrive while this screen is open.
  useEffect(() => {
    if (!gateway?.enabled) return;
    const id = window.setInterval(() => void api.gateway().then(setGateway).catch(() => {}), 4000);
    return () => window.clearInterval(id);
  }, [gateway?.enabled]);

  const save = useCallback(
    async (patch: Record<string, unknown>, message = 'Saved.') => {
      setBusy(true);
      setError(null);
      try {
        const result = await api.saveSettings(patch);
        setSettings(result.settings);
        setNote(
          result.restart_required
            ? 'Saved. Quit and reopen Wired for that one to take effect.'
            : result.restart_assistant
              ? 'Saved. Starting your assistant again so it takes effect…'
              : message,
        );
        onChanged();
        // The running CLI cannot adopt this one, so bring the session back up as
        // whatever was just saved — not as whatever happens to be running.
        if (result.restart_assistant) onRestartAssistant(result.settings.assistant);
        return true;
      } catch (err) {
        setError(briefly(err));
        return false;
      } finally {
        setBusy(false);
        window.setTimeout(() => setNote(null), 4000);
      }
    },
    [onChanged, onRestartAssistant],
  );

  if (!settings) {
    return <div className="screen-empty">{error ?? 'Loading your settings…'}</div>;
  }

  const locked = (field: string) => settings.env_overrides.includes(field);

  return (
    <div className="screen">
      <div className="screen-inner">
        <h2 className="screen-title">Settings</h2>

        {note && <div className="toast toast-success">{note}</div>}
        {error && (
          <div className="banner" role="alert">
            <span>{error}</span>
            <button type="button" className="btn btn-ghost" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        )}

        <section className="card">
          <h3>Your assistant</h3>
          <Row
            label="Which one"
            hint={locked('assistant') ? 'Set outside the app; this control is read-only.' : undefined}
          >
            <select
              className="field"
              value={settings.assistant}
              disabled={busy || locked('assistant')}
              onChange={(e) => void save({ assistant: e.target.value as ProviderId })}
            >
              {PROVIDERS.filter((p) => p.isAgent).map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </Row>

          <Row label="Its folder" hint="The only place it may read and write.">
            <FolderPicker
              suggested={settings.folder}
              busy={busy || locked('folder')}
              label="Use this"
              onChoose={(folder) => void save({ folder })}
            />
          </Row>

          <Toggle
            label="Always on"
            hint="Keep it running and start it again if it stops."
            checked={settings.always_on}
            disabled={busy || locked('always_on')}
            onChange={(value) => void save({ always_on: value })}
          />

          <Toggle
            label="Start when I log in"
            hint="So it is already there in the morning."
            checked={settings.start_at_login}
            disabled={busy}
            onChange={(value) =>
              void desktop
                .setLoginItem(value)
                .then((actual) => save({ start_at_login: actual }))
            }
          />
        </section>

        <TelegramCard
          gateway={gateway}
          onChanged={(next) => {
            setGateway(next);
            onChanged();
          }}
          onError={setError}
        />

        <section className="card">
          <h3>Advanced</h3>
          <p className="hint">
            Wired is listening on port {settings.port}. Passwords are kept in{' '}
            {settings.secrets === 'keychain' ? 'your keychain' : 'a private file'}.
          </p>
          <div className="button-row">
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => void desktop.openPath(settings.config_file)}
            >
              Open the settings file
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => void desktop.openPath(settings.log_file)}
            >
              Open the log
            </button>
          </div>
        </section>
      </div>
    </div>
  );
}

function Row({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="setting-row">
      <div className="setting-label">
        <span>{label}</span>
        {hint && <span className="hint">{hint}</span>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

function Toggle({
  label,
  hint,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className="setting-row setting-toggle">
      <div className="setting-label">
        <span>{label}</span>
        {hint && <span className="hint">{hint}</span>}
      </div>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
    </label>
  );
}

/**
 * Three steps: paste the token BotFather gave you, message the bot, approve the
 * code it reads back. That is the whole of "reach it from my phone" — no port
 * opened, no tunnel, nothing typed into a URL bar.
 */
function TelegramCard({
  gateway,
  onChanged,
  onError,
}: {
  gateway: GatewayStatus | null;
  onChanged: (next: GatewayStatus) => void;
  onError: (message: string) => void;
}) {
  const [token, setToken] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);

  const call = useCallback(
    async (action: () => Promise<{ gateway: GatewayStatus } | unknown>) => {
      setBusy(true);
      try {
        await action();
        onChanged(await api.gateway());
      } catch (err) {
        onError(briefly(err));
      } finally {
        setBusy(false);
      }
    },
    [onChanged, onError],
  );

  if (!gateway) return null;

  return (
    <section className="card">
      <h3>Talk to it from your phone</h3>

      {!gateway.configured ? (
        <>
          <ol className="plain-list numbered">
            <li>
              Open Telegram and message <strong>@BotFather</strong>. Send it{' '}
              <code>/newbot</code> and answer its two questions.
            </li>
            <li>It replies with a long code. Paste that here.</li>
          </ol>
          <div className="button-row">
            <input
              className="field"
              type="password"
              value={token}
              placeholder="Paste the code from BotFather"
              spellCheck={false}
              onChange={(e) => setToken(e.target.value)}
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !token.trim()}
              onClick={() =>
                void call(async () => {
                  await api.configureGateway({ bot_token: token.trim(), enabled: true });
                  setToken('');
                })
              }
            >
              Connect
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="status-line">
            <span className="pill-dot" data-glow={gateway.connected} />
            {gateway.connected
              ? `Connected as ${gateway.bot ?? 'your bot'}.`
              : gateway.last_error
                ? `Not connected: ${gateway.last_error}`
                : 'Connecting…'}
          </p>

          {gateway.paired_chats === 0 ? (
            <p>
              Now open Telegram, find {gateway.bot ?? 'your bot'}, and send it anything. It will
              give you a code to approve below.
            </p>
          ) : (
            <p>
              {gateway.paired_chats} {gateway.paired_chats === 1 ? 'phone is' : 'phones are'} paired.
            </p>
          )}

          {gateway.pending.length > 0 && (
            <div className="pairing-list">
              {gateway.pending.map((request) => (
                <div key={request.code} className="pairing">
                  <div>
                    <strong>{request.display}</strong>
                    <span className="pairing-code">{request.code}</span>
                    <span className="hint">
                      expires in {Math.max(1, Math.round(request.expires_in / 60))} min
                    </span>
                  </div>
                  <div className="button-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={busy}
                      onClick={() => void call(() => api.approvePairing(request.code))}
                    >
                      Let them in
                    </button>
                    <button
                      type="button"
                      className="btn btn-ghost"
                      disabled={busy}
                      onClick={() => void call(() => api.denyPairing(request.code))}
                    >
                      No
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}

          {gateway.locked_for != null && (
            <p className="hint">
              Too many wrong codes. Approving is locked for{' '}
              {Math.ceil(gateway.locked_for / 60)} more minutes.
            </p>
          )}

          <details>
            <summary>Have a code to type in?</summary>
            <div className="button-row">
              <input
                className="field"
                value={code}
                placeholder="8 characters"
                maxLength={8}
                spellCheck={false}
                onChange={(e) => setCode(e.target.value.toUpperCase())}
              />
              <button
                type="button"
                className="btn btn-secondary"
                disabled={busy || code.length < 8}
                onClick={() =>
                  void call(async () => {
                    await api.approvePairing(code);
                    setCode('');
                  })
                }
              >
                Approve
              </button>
            </div>
          </details>

          <div className="button-row">
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy}
              onClick={() => void call(() => api.configureGateway({ muted: !gateway.muted }))}
            >
              {gateway.muted ? 'Send replies to my phone again' : 'Stop sending replies to my phone'}
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={() => void call(() => api.configureGateway({ enabled: !gateway.enabled }))}
            >
              {gateway.enabled ? 'Turn Telegram off' : 'Turn Telegram on'}
            </button>
          </div>

          {/* The way back to the paste-a-token screen. Needed whenever the code
              was mistyped, the bot was deleted in BotFather, or the token was
              seen by someone it should not have been. */}
          {confirmReset ? (
            <div className="button-row">
              <button
                type="button"
                className="btn btn-danger"
                disabled={busy}
                onClick={() =>
                  void call(async () => {
                    await api.resetGateway();
                    setConfirmReset(false);
                  })
                }
              >
                Yes, forget the bot
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                disabled={busy}
                onClick={() => setConfirmReset(false)}
              >
                Cancel
              </button>
            </div>
          ) : (
            <details>
              <summary>Use a different bot</summary>
              <p className="hint">
                Forgets the code from BotFather and unpairs every phone, so you can start again
                with a new bot. Nothing else on this computer changes.
              </p>
              <button
                type="button"
                className="btn btn-secondary"
                disabled={busy}
                onClick={() => setConfirmReset(true)}
              >
                Forget this bot
              </button>
            </details>
          )}
        </>
      )}
    </section>
  );
}
