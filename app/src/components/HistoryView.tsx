import { useCallback, useEffect, useState } from 'react';
import { api, type HistoryEvent } from '../lib/api';
import { briefly } from '../lib/errors';

/**
 * "What did you do last night?"
 *
 * The live transcript was an in-memory buffer: a keep-alive restart reset it and
 * quitting lost everything, which made an always-on assistant indistinguishable
 * from an idle one. The backend now appends each day to its own file; this reads
 * them back.
 */

const KIND_LABEL: Record<string, string> = {
  user: 'You',
  text: 'Assistant',
  prompt: 'Asked you',
  notice: 'Notice',
  system: 'Wired',
};

function time(ts: number): string {
  return new Date(ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function readableDay(day: string): string {
  const date = new Date(`${day}T12:00:00`);
  const today = new Date();
  const isSame = (a: Date, b: Date) => a.toDateString() === b.toDateString();
  const yesterday = new Date(today.getTime() - 86_400_000);
  if (isSame(date, today)) return 'Today';
  if (isSame(date, yesterday)) return 'Yesterday';
  return date.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'long' });
}

export function HistoryView({ onAskAgain }: { onAskAgain: (text: string) => void }) {
  const [days, setDays] = useState<string[]>([]);
  const [day, setDay] = useState<string | null>(null);
  const [events, setEvents] = useState<HistoryEvent[]>([]);
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<{ day: string; event: HistoryEvent }[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .historyDays()
      .then((result) => {
        setDays(result.days);
        setDay((current) => current ?? result.days[0] ?? null);
      })
      .catch((err) => setError(briefly(err)));
  }, []);

  useEffect(() => {
    if (!day) return;
    api
      .historyDay(day)
      .then((result) => setEvents(result.events))
      .catch((err) => setError(briefly(err)));
  }, [day]);

  const search = useCallback(async () => {
    const text = query.trim();
    if (!text) {
      setHits(null);
      return;
    }
    try {
      const result = await api.historySearch(text);
      setHits(result.hits);
    } catch (err) {
      setError(briefly(err));
    }
  }, [query]);

  if (error) return <div className="screen-empty">{error}</div>;

  return (
    <div className="screen">
      <div className="screen-inner">
        <h2 className="screen-title">History</h2>

        <div className="history-controls">
          <input
            className="field"
            value={query}
            placeholder="Search everything it has said…"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void search();
              if (e.key === 'Escape') {
                setQuery('');
                setHits(null);
              }
            }}
          />
          <button type="button" className="btn btn-secondary" onClick={() => void search()}>
            Search
          </button>
        </div>

        {hits === null ? (
          <>
            <div className="day-tabs">
              {days.length === 0 && <span className="hint">Nothing saved yet.</span>}
              {days.map((d) => (
                <button
                  key={d}
                  type="button"
                  className="day-tab"
                  aria-pressed={d === day}
                  data-selected={d === day}
                  onClick={() => setDay(d)}
                >
                  {readableDay(d)}
                </button>
              ))}
            </div>

            <div className="history-list">
              {events.length === 0 && day && (
                <p className="hint">Nothing was recorded on this day.</p>
              )}
              {events.map((event) => (
                <HistoryRow key={event.seq} event={event} onAskAgain={onAskAgain} />
              ))}
            </div>
          </>
        ) : (
          <div className="history-list">
            {hits.length === 0 && <p className="hint">No matches.</p>}
            {hits.map(({ day: hitDay, event }) => (
              <div key={`${hitDay}-${event.seq}`}>
                <span className="history-day-marker">{readableDay(hitDay)}</span>
                <HistoryRow event={event} onAskAgain={onAskAgain} />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function HistoryRow({
  event,
  onAskAgain,
}: {
  event: HistoryEvent;
  onAskAgain: (text: string) => void;
}) {
  return (
    <div className="history-row" data-kind={event.kind}>
      <span className="history-meta">
        {time(event.ts)} · {KIND_LABEL[event.kind] ?? event.kind}
      </span>
      <span className="history-text">{event.text}</span>
      {event.kind === 'user' && (
        <button type="button" className="btn btn-ghost" onClick={() => onAskAgain(event.text)}>
          Ask again
        </button>
      )}
    </div>
  );
}
