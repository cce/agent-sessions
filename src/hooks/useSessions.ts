import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Session, SessionsResponse, ItermLayoutResponse } from '../types/session';

const POLL_INTERVAL = 2000; // Fallback polling interval (ms)

// Get ordering priority for card stability (only distinguishes active vs idle)
// This prevents card reordering when status flips between thinking/processing/waiting
function getOrderingPriority(status: string): number {
  switch (status) {
    case 'thinking':
    case 'processing':
    case 'waiting':
      return 0; // All active states - same ordering priority
    case 'idle':
      return 1; // Only idle causes reordering
    default:
      return 2;
  }
}

// Merge new sessions with existing order, only reordering when priority changes
function mergeWithStableOrder(existing: Session[], incoming: Session[]): Session[] {
  if (existing.length === 0) {
    return incoming;
  }

  const existingOrder = new Map<string, number>();
  existing.forEach((s, idx) => existingOrder.set(s.id, idx));

  const existingPriority = new Map<string, number>();
  existing.forEach(s => existingPriority.set(s.id, getOrderingPriority(s.status)));

  let priorityChanged = false;
  for (const session of incoming) {
    const oldPriority = existingPriority.get(session.id);
    const newPriority = getOrderingPriority(session.status);
    if (oldPriority !== undefined && oldPriority !== newPriority) {
      priorityChanged = true;
      break;
    }
  }

  const hasNewSessions = incoming.some(s => !existingOrder.has(s.id));
  const hasRemovedSessions = existing.some(s => !incoming.find(i => i.id === s.id));

  if (priorityChanged || hasNewSessions || hasRemovedSessions) {
    return incoming;
  }

  const incomingMap = new Map<string, Session>();
  incoming.forEach(s => incomingMap.set(s.id, s));

  const result: Session[] = [];
  for (const existingSession of existing) {
    const updated = incomingMap.get(existingSession.id);
    if (updated) {
      result.push(updated);
      incomingMap.delete(existingSession.id);
    }
  }

  for (const newSession of incomingMap.values()) {
    result.push(newSession);
  }

  return result;
}

export function useSessions() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [waitingCount, setWaitingCount] = useState(0);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const sessionsRef = useRef<Session[]>([]);

  // Window grouping state
  const [groupByWindow, setGroupByWindow] = useState(false);
  const [itermLayout, setItermLayout] = useState<ItermLayoutResponse | null>(null);
  const [layoutError, setLayoutError] = useState<string | null>(null);

  const updateTrayTitle = useCallback(async (total: number, waiting: number) => {
    try {
      await invoke('update_tray_title', { total, waiting });
    } catch (err) {
      console.error('Failed to update tray title:', err);
    }
  }, []);

  const applySessionsResponse = useCallback(async (response: SessionsResponse) => {
    const stableSessions = mergeWithStableOrder(sessionsRef.current, response.sessions);
    sessionsRef.current = stableSessions;
    setSessions([...stableSessions]);
    setTotalCount(response.totalCount);
    setWaitingCount(response.waitingCount);
    setError(null);
    setIsLoading(false);
    await updateTrayTitle(response.totalCount, response.waitingCount);
  }, [updateTrayTitle]);

  const fetchSessions = useCallback(async () => {
    try {
      const response = await invoke<SessionsResponse>('get_all_sessions');
      await applySessionsResponse(response);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to fetch sessions');
      setIsLoading(false);
    }
  }, [applySessionsResponse]);

  const focusSession = useCallback(async (session: Session) => {
    try {
      await invoke('focus_session', {
        pid: session.pid,
        projectPath: session.projectPath,
      });
    } catch (err) {
      console.error('Failed to focus session:', err);
    }
  }, []);

  // Initial fetch
  useEffect(() => {
    fetchSessions();
  }, [fetchSessions]);

  // Subscribe to backend push events (primary update mechanism)
  useEffect(() => {
    const unlistenPromise = listen<SessionsResponse>('sessions-updated', (event) => {
      applySessionsResponse(event.payload);
    });

    return () => {
      unlistenPromise.then(fn => fn());
    };
  }, [applySessionsResponse]);

  // Fallback polling in case events are missed
  useEffect(() => {
    const interval = setInterval(fetchSessions, POLL_INTERVAL);
    return () => clearInterval(interval);
  }, [fetchSessions]);

  // Report visibility tier to backend for adaptive polling
  useEffect(() => {
    const reportVisibility = () => {
      const tier = document.visibilityState === 'hidden'
        ? 'background_hidden'
        : 'foreground';
      invoke('set_visibility_tier', { tier }).catch(() => {});
    };

    // Report on visibility change
    document.addEventListener('visibilitychange', reportVisibility);

    // Report on window focus/blur via Tauri API
    let unlistenFocus: (() => void) | null = null;

    const appWindow = getCurrentWindow();
    appWindow.onFocusChanged(({ payload: focused }) => {
      const tier = focused ? 'foreground' : 'background_visible';
      invoke('set_visibility_tier', { tier }).catch(() => {});
    }).then(unlisten => {
      unlistenFocus = unlisten;
    });

    // Set initial state
    reportVisibility();

    return () => {
      document.removeEventListener('visibilitychange', reportVisibility);
      if (unlistenFocus) unlistenFocus();
    };
  }, []);

  // Fetch iTerm2 layout when grouping is enabled
  const fetchItermLayout = useCallback(async () => {
    try {
      const layout = await invoke<ItermLayoutResponse>('get_iterm_layout');
      setItermLayout(layout);
      setLayoutError(null);
    } catch (err) {
      console.error('Failed to fetch iTerm2 layout:', err);
      const errorMsg = typeof err === 'string' ? err :
        (err instanceof Error ? err.message : JSON.stringify(err));
      setLayoutError(errorMsg || 'Failed to fetch iTerm2 layout');
      setItermLayout(null);
    }
  }, []);

  useEffect(() => {
    if (groupByWindow) {
      fetchItermLayout();
      const interval = setInterval(fetchItermLayout, POLL_INTERVAL);
      return () => clearInterval(interval);
    } else {
      setItermLayout(null);
      setLayoutError(null);
    }
  }, [groupByWindow, fetchItermLayout]);

  return {
    sessions,
    totalCount,
    waitingCount,
    isLoading,
    error,
    refresh: fetchSessions,
    focusSession,
    groupByWindow,
    setGroupByWindow,
    itermLayout,
    layoutError,
  };
}
