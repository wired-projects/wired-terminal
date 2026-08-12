import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * The message box, under the conversation where it belongs.
 *
 * Enter sends and Shift+Enter makes a new line, which is what every chat app on
 * the machine already does. It used to be the other way round, inherited from a
 * sidebar that described itself as "Command your agent".
 */

interface ComposerProps {
  onSend: (text: string) => Promise<boolean>;
  disabled?: boolean;
  /** Shown under the box: the one folder the assistant may touch. */
  folder?: string;
  placeholder?: string;
  /** Set by a starter prompt or the History view's "ask again". */
  prefill?: string;
}

const MAX_ROWS = 8;

export function Composer({ onSend, disabled, folder, placeholder, prefill }: ComposerProps) {
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const box = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (prefill === undefined) return;
    setDraft(prefill);
    box.current?.focus();
  }, [prefill]);

  // Grow with the text, then scroll — a fixed six-row box is mostly empty and
  // then suddenly too small.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = 'auto';
    const line = parseFloat(getComputedStyle(el).lineHeight) || 20;
    el.style.height = `${Math.min(el.scrollHeight, line * MAX_ROWS + 20)}px`;
  }, [draft]);

  const send = useCallback(async () => {
    const text = draft.trim();
    if (!text || busy) return;
    setBusy(true);
    try {
      if (await onSend(text)) setDraft('');
    } finally {
      setBusy(false);
    }
  }, [draft, busy, onSend]);

  return (
    <div className="composer">
      <div className="composer-box">
        <label className="sr-only" htmlFor="composer-input">
          Message your assistant
        </label>
        <textarea
          id="composer-input"
          ref={box}
          className="composer-input"
          rows={1}
          value={draft}
          disabled={disabled}
          placeholder={placeholder ?? 'Ask your assistant to do something…'}
          spellCheck
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== 'Enter' || e.shiftKey) return;
            // Leave IME composition alone: Enter is confirming a candidate.
            if (e.nativeEvent.isComposing) return;
            e.preventDefault();
            void send();
          }}
        />
        <button
          type="button"
          className="btn btn-primary composer-send"
          disabled={!draft.trim() || busy || disabled}
          onClick={() => void send()}
        >
          {busy ? 'Sending…' : 'Send'}
        </button>
      </div>
      <div className="composer-foot">
        <span>
          <span className="kbd">Enter</span> to send · <span className="kbd">Shift+Enter</span> for a
          new line
        </span>
        {folder && (
          <span className="composer-scope" title="The only folder your assistant may read and write">
            Working in {folder}
          </span>
        )}
      </div>
    </div>
  );
}
