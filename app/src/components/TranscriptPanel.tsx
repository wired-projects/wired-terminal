import { useCallback, useEffect, useRef, useState } from 'react';
import { streamUrl } from '../lib/api';

/**
 * The conversation, rendered.
 *
 * The terminal view shows what the CLI paints; this shows what was *said*. It
 * consumes the same SSE feed as `curl -N /api/agent/output/stream`, where the
 * backend has already dropped banners, spinners and status bars.
 *
 * Approval prompts get **Allow** and **Don't** buttons. They used to be
 * rendered and then followed by an instruction to send a POST — an agent
 * blocked on a question nobody in the room could answer.
 */

type RowKind = 'user' | 'agent' | 'action' | 'prompt' | 'notice' | 'divider' | 'system';

interface Row {
  id: number;
  kind: RowKind;
  text: string;
  /** Leading glyph for `action` rows, e.g. ⏺ */
  mark?: string;
  /** Set once the prompt has been answered here. */
  answered?: 'allow' | 'deny';
}

// Bounded so a long-running session cannot grow the DOM without limit.
const MAX_ROWS = 2000;
// The CLIs print progress rows while working; they are activity, not prose.
const ACTION_RE = /^([◆⏺●▪▸])\s+(.*)$/;

interface TranscriptPanelProps {
  sessionKey: number;
  onAnswer?: (allow: boolean) => Promise<boolean>;
  /** Rendered in place of the empty state — the starter prompts. */
  empty?: React.ReactNode;
  /** Fired whenever a row arrives, so the header can say "Working on it…". */
  onActivity?: () => void;
}

export function TranscriptPanel({ sessionKey, onAnswer, empty, onActivity }: TranscriptPanelProps) {
  const [rows, setRows] = useState<Row[]>([]);
  const [connected, setConnected] = useState(false);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const nextId = useRef(0);

  const activity = useRef(onActivity);
  activity.current = onActivity;

  const append = useCallback((kind: RowKind, text: string, mark?: string) => {
    activity.current?.();
    setRows((prev) => {
      const next = [...prev, { id: nextId.current++, kind, text, mark }];
      return next.length > MAX_ROWS ? next.slice(next.length - MAX_ROWS) : next;
    });
  }, []);

  // Follow the tail unless the reader has scrolled up to look at something.
  const onScroll = useCallback(() => {
    const el = scrollerRef.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  }, []);

  useEffect(() => {
    if (!stickRef.current) return;
    const el = scrollerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [rows]);

  useEffect(() => {
    setRows([]);
    const source = new EventSource(streamUrl());

    source.onopen = () => setConnected(true);
    source.onerror = () => setConnected(false);

    source.onmessage = (event) => {
      // A user turn arrives as a blank data line then "❯ text"; the blank is
      // the paragraph break that makes the curl output readable.
      const text = event.data.replace(/^\n+/, '');
      if (!text.trim()) return;
      if (text.startsWith('❯ ')) {
        append('user', text.slice(2));
        return;
      }
      const action = ACTION_RE.exec(text);
      if (action) append('action', action[2], action[1]);
      else append('agent', text);
    };

    // `text` and `user` rows arrive as unnamed `message` events — see `sse()`
    // in routes.rs, which keeps that shape so `curl -N` stays readable.
    const named = (kind: RowKind) => (e: MessageEvent) => append(kind, e.data);
    source.addEventListener('prompt', named('prompt'));
    source.addEventListener('notice', named('notice'));
    source.addEventListener('system', named('system'));
    source.addEventListener('status', named('divider'));
    source.addEventListener('session', named('divider'));

    return () => source.close();
  }, [append, sessionKey]);

  // Only the newest unanswered prompt is still live; anything above it has
  // either been answered or been overtaken.
  const livePromptId = (() => {
    for (let i = rows.length - 1; i >= 0; i -= 1) {
      if (rows[i].kind === 'prompt') return rows[i].answered ? null : rows[i].id;
    }
    return null;
  })();

  const answer = useCallback(
    async (id: number, allow: boolean) => {
      if (!onAnswer) return;
      const ok = await onAnswer(allow);
      if (!ok) return;
      setRows((prev) =>
        prev.map((row) => (row.id === id ? { ...row, answered: allow ? 'allow' : 'deny' } : row)),
      );
    },
    [onAnswer],
  );

  return (
    <div className="panel-fill">
      <div className="transcript" ref={scrollerRef} onScroll={onScroll}>
        <div className="transcript-inner">
          {rows.length === 0 &&
            (empty ?? (
              <div className="transcript-empty">
                {connected ? 'Waiting for your assistant…' : 'Connecting…'}
              </div>
            ))}
          {rows.map((row) => (
            <TranscriptRow
              key={row.id}
              row={row}
              live={row.id === livePromptId}
              onAnswer={answer}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function TranscriptRow({
  row,
  live,
  onAnswer,
}: {
  row: Row;
  live: boolean;
  onAnswer: (id: number, allow: boolean) => void;
}) {
  switch (row.kind) {
    case 'user':
      return (
        <div className="turn-user">
          <span className="caret" aria-hidden="true">
            ❯
          </span>
          <span className="turn-line">{row.text}</span>
        </div>
      );
    case 'action':
      return (
        <div className="turn-agent">
          <div className="turn-action">
            <span className="mark" aria-hidden="true">
              {row.mark}
            </span>
            <span>{row.text}</span>
          </div>
        </div>
      );
    case 'prompt':
      return (
        <div className="turn-prompt" role="alert">
          <span className="label">needs your answer</span>
          <div className="turn-line">{row.text}</div>
          {live ? (
            <div className="prompt-actions">
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => onAnswer(row.id, true)}
              >
                Allow
              </button>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => onAnswer(row.id, false)}
              >
                Don't
              </button>
            </div>
          ) : (
            row.answered && (
              <div className="prompt-answered">
                {row.answered === 'allow' ? 'You allowed this.' : "You said don't."}
              </div>
            )
          )}
        </div>
      );
    case 'notice':
      return <div className="turn-notice">{row.text}</div>;
    case 'system':
      return <div className="turn-system">{row.text}</div>;
    case 'divider':
      return <div className="turn-divider">{row.text}</div>;
    default:
      return (
        <div className="turn-agent">
          <div className="turn-line">{row.text}</div>
        </div>
      );
  }
}
