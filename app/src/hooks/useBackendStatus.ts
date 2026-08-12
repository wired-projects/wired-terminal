import { useCallback, useEffect, useRef, useState } from 'react';
import { api, type HealthResponse, type ProviderId } from '../lib/api';

const POLL_INTERVAL_MS = 5_000;
// The desktop app starts the backend as it opens, so the first probes land
// before it is listening. Retry fast until it answers, then settle down.
const OFFLINE_RETRY_MS = 1_000;

export interface SessionState {
  running: boolean;
  provider: ProviderId | null;
  generation: number | null;
}

/**
 * Tracks whether the backend is up, which CLIs it can see, and what session it
 * is currently running. Polls health on an interval so the UI recovers on its
 * own when the backend is restarted underneath it.
 *
 * The whole health payload is kept, not just the flags the old sidebar needed:
 * the waiting-approval badge, the setup wizard and the visible folder scope all
 * read from it, and one poll is cheaper than three.
 */
export function useBackendStatus() {
  const [online, setOnline] = useState<boolean | null>(null);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [available, setAvailable] = useState<Record<string, boolean>>({});
  const [keepAlive, setKeepAlive] = useState(true);
  const [session, setSession] = useState<SessionState>({
    running: false,
    provider: null,
    generation: null,
  });
  const [preferredProvider, setPreferredProvider] = useState<ProviderId>('claude');

  // Avoids a state update after unmount when a poll is in flight.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const probe = useCallback(async (): Promise<boolean> => {
    try {
      const next = await api.health();
      if (!mounted.current) return true;

      setOnline(true);
      setHealth(next);
      setAvailable(
        Object.fromEntries(Object.entries(next.providers).map(([k, v]) => [k, !!v.available])),
      );
      if (next.assistant?.provider) setPreferredProvider(next.assistant.provider);
      if (typeof next.assistant?.keep_alive === 'boolean') {
        setKeepAlive(next.assistant.keep_alive);
      }
      // health already knows whether a session is alive; skipping the second
      // request keeps the poll to one round trip.
      setSession((prev) => ({
        running: !!next.assistant?.session_running,
        provider: next.assistant?.session_provider ?? prev.provider,
        generation: next.assistant?.generation ?? prev.generation,
      }));
      return true;
    } catch {
      if (mounted.current) setOnline(false);
      return false;
    }
  }, []);

  const sync = probe;

  useEffect(() => {
    void probe();
  }, [probe]);

  useEffect(() => {
    const id = window.setInterval(
      () => void probe(),
      online ? POLL_INTERVAL_MS : OFFLINE_RETRY_MS,
    );
    return () => window.clearInterval(id);
  }, [online, probe]);

  return {
    online,
    health,
    available,
    keepAlive,
    session,
    setSession,
    preferredProvider,
    probe,
    sync,
  };
}
