import { useCallback, useEffect, useState } from 'react';
import { api, type Schedule } from '../lib/api';
import { briefly } from '../lib/errors';

/**
 * Scheduling, in sentences.
 *
 * The 24/7 feature used to be documented as a crontab line calling a bash
 * script. "every morning at 8" is the same capability written in a language he
 * reads — and cron expressions still parse, for anyone who prefers them.
 *
 * The examples are pre-filled deliberately: a feature that explains itself gets
 * used, and the first schedule is always the hardest one to imagine.
 */

const EXAMPLES: { name: string; when: string; task: string }[] = [
  {
    name: 'Morning briefing',
    when: 'every morning at 8',
    task: 'Look through my folder and tell me anything that changed since yesterday. Keep it short.',
  },
  {
    name: 'Tidy up',
    when: 'every friday at 5pm',
    task: 'Find files in my folder that look like duplicates or leftovers and list them. Do not delete anything.',
  },
  {
    name: 'Hourly check',
    when: 'every hour',
    task: 'Check whether anything in my folder needs attention. If nothing does, say so.',
  },
];

const BLANK: Partial<Schedule> = {
  name: '',
  when: 'every morning at 8',
  task: '',
  enabled: true,
  quiet_when_nothing: true,
};

export function ScheduleView() {
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [running, setRunning] = useState<string | null>(null);
  const [draft, setDraft] = useState<Partial<Schedule> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const result = await api.schedules();
      setSchedules(result.schedules);
      setRunning(result.running);
    } catch (err) {
      setError(briefly(err));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = useCallback(
    async (action: () => Promise<unknown>) => {
      setBusy(true);
      setError(null);
      try {
        await action();
        await load();
        return true;
      } catch (err) {
        setError(briefly(err));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  return (
    <div className="screen">
      <div className="screen-inner">
        <h2 className="screen-title">Things it does on its own</h2>

        {error && (
          <div className="banner" role="alert">
            <span>{error}</span>
            <button type="button" className="btn btn-ghost" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        )}

        {schedules.length === 0 && !draft && (
          <section className="card">
            <p>
              Nothing is scheduled yet. Pick one of these to start with, or write your own — you can
              change it afterwards.
            </p>
            <div className="starter-grid">
              {EXAMPLES.map((example) => (
                <button
                  key={example.name}
                  type="button"
                  className="starter-card"
                  onClick={() => setDraft({ ...BLANK, ...example })}
                >
                  <span className="name">{example.name}</span>
                  <span className="blurb">{example.when}</span>
                </button>
              ))}
            </div>
          </section>
        )}

        {schedules.map((schedule) => (
          <section className="card schedule" key={schedule.id}>
            <div className="schedule-head">
              <div>
                <h3>{schedule.name}</h3>
                <span className="hint">
                  {schedule.when_readable}
                  {schedule.enabled && schedule.next_readable
                    ? ` · next ${schedule.next_readable}`
                    : ' · paused'}
                </span>
              </div>
              <div className="button-row">
                <button
                  type="button"
                  className="btn btn-secondary"
                  disabled={busy || running !== null}
                  onClick={() => void run(() => api.runSchedule(schedule.id))}
                >
                  {running === schedule.id ? 'Running…' : 'Run it now'}
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={busy}
                  onClick={() =>
                    void run(() =>
                      api.saveSchedule({ ...schedule, enabled: !schedule.enabled }),
                    )
                  }
                >
                  {schedule.enabled ? 'Pause' : 'Resume'}
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={busy}
                  onClick={() => setDraft(schedule)}
                >
                  Edit
                </button>
              </div>
            </div>
            <p className="schedule-task">{schedule.task}</p>
            {schedule.last_result && (
              <details>
                <summary>
                  Last time
                  {schedule.last_run
                    ? ` · ${new Date(schedule.last_run * 1000).toLocaleString()}`
                    : ''}
                </summary>
                <pre className="log-dump">
                  {schedule.last_result === '[SILENT]'
                    ? 'Nothing to report — so it sent you nothing.'
                    : schedule.last_result}
                </pre>
              </details>
            )}
          </section>
        ))}

        {draft ? (
          <ScheduleForm
            draft={draft}
            busy={busy}
            onCancel={() => setDraft(null)}
            onSave={(next) =>
              void run(() => api.saveSchedule(next)).then((ok) => ok && setDraft(null))
            }
            onDelete={
              draft.id
                ? () =>
                    void run(() => api.deleteSchedule(draft.id!)).then(
                      (ok) => ok && setDraft(null),
                    )
                : undefined
            }
          />
        ) : (
          <button type="button" className="btn btn-primary" onClick={() => setDraft({ ...BLANK })}>
            Add something
          </button>
        )}
      </div>
    </div>
  );
}

function ScheduleForm({
  draft,
  busy,
  onSave,
  onCancel,
  onDelete,
}: {
  draft: Partial<Schedule>;
  busy: boolean;
  onSave: (schedule: Partial<Schedule>) => void;
  onCancel: () => void;
  onDelete?: () => void;
}) {
  const [form, setForm] = useState(draft);
  useEffect(() => setForm(draft), [draft]);

  const set = (patch: Partial<Schedule>) => setForm((prev) => ({ ...prev, ...patch }));

  return (
    <section className="card">
      <h3>{draft.id ? 'Edit' : 'Something new'}</h3>

      <label className="field-label">
        What should it do?
        <textarea
          className="field"
          rows={3}
          value={form.task ?? ''}
          placeholder="Tell me if anything in my folder changed overnight."
          onChange={(e) => set({ task: e.target.value })}
        />
      </label>

      <label className="field-label">
        When?
        <input
          className="field"
          value={form.when ?? ''}
          placeholder="every morning at 8"
          onChange={(e) => set({ when: e.target.value })}
        />
      </label>
      <p className="hint">
        Try <code>every hour</code>, <code>every morning at 8</code>,{' '}
        <code>every monday at 9am</code>, or a cron expression.
      </p>

      <label className="field-label">
        Call it something (optional)
        <input
          className="field"
          value={form.name ?? ''}
          placeholder="Morning briefing"
          onChange={(e) => set({ name: e.target.value })}
        />
      </label>

      <label className="checkbox">
        <input
          type="checkbox"
          checked={form.quiet_when_nothing ?? true}
          onChange={(e) => set({ quiet_when_nothing: e.target.checked })}
        />
        Say nothing when there is nothing to report
      </label>

      <div className="button-row">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !form.task?.trim()}
          onClick={() => onSave(form)}
        >
          Save
        </button>
        <button type="button" className="btn btn-ghost" onClick={onCancel}>
          Cancel
        </button>
        {onDelete && (
          <button type="button" className="btn btn-danger" disabled={busy} onClick={onDelete}>
            Delete
          </button>
        )}
      </div>
    </section>
  );
}
