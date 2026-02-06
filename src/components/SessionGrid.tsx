import { Session, ItermLayoutResponse } from '../types/session';
import { SessionCard } from './SessionCard';

interface SessionGridProps {
  sessions: Session[];
  onSessionClick: (session: Session) => void;
  groupByWindow?: boolean;
  itermLayout?: ItermLayoutResponse | null;
}

interface WindowGroup {
  windowId: string;
  sessions: Session[];
}

function groupSessionsByWindow(
  sessions: Session[],
  layout: ItermLayoutResponse
): WindowGroup[] {
  const groups = new Map<string, Session[]>();
  const ungrouped: Session[] = [];

  for (const session of sessions) {
    if (session.tty) {
      const windowId = layout.sessionToWindow[session.tty];
      if (windowId) {
        const existing = groups.get(windowId) || [];
        existing.push(session);
        groups.set(windowId, existing);
      } else {
        ungrouped.push(session);
      }
    } else {
      ungrouped.push(session);
    }
  }

  const result: WindowGroup[] = [];

  // Add grouped sessions first
  for (const [windowId, windowSessions] of groups) {
    result.push({ windowId, sessions: windowSessions });
  }

  // Add ungrouped sessions as a separate group if any
  if (ungrouped.length > 0) {
    result.push({ windowId: 'ungrouped', sessions: ungrouped });
  }

  return result;
}

export function SessionGrid({ sessions, onSessionClick, groupByWindow, itermLayout }: SessionGridProps) {
  if (groupByWindow && itermLayout) {
    const grouped = groupSessionsByWindow(sessions, itermLayout);

    return (
      <div className="space-y-6">
        {grouped.map(({ windowId, sessions: windowSessions }) => (
          <div key={windowId}>
            <h3 className="text-sm font-medium text-muted-foreground mb-3 flex items-center gap-2">
              <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 17V7m0 10a2 2 0 01-2 2H5a2 2 0 01-2-2V7a2 2 0 012-2h2a2 2 0 012 2m0 10a2 2 0 002 2h2a2 2 0 002-2M9 7a2 2 0 012-2h2a2 2 0 012 2m0 10V7m0 10a2 2 0 002 2h2a2 2 0 002-2V7a2 2 0 00-2-2h-2a2 2 0 00-2 2" />
              </svg>
              {windowId === 'ungrouped' ? 'Other Sessions' : `Window ${windowId.slice(-4)}`}
              <span className="text-xs opacity-60">({windowSessions.length})</span>
            </h3>
            <div className="flex flex-wrap gap-4">
              {windowSessions.map((session) => (
                <div key={`${session.id}-${session.pid}`} className="w-full sm:w-[calc(50%-0.5rem)] lg:w-[calc(33.333%-0.667rem)]">
                  <SessionCard
                    session={session}
                    onClick={() => onSessionClick(session)}
                  />
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    );
  }

  // Default grid view
  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
      {sessions.map((session) => (
        <SessionCard
          key={`${session.id}-${session.pid}`}
          session={session}
          onClick={() => onSessionClick(session)}
        />
      ))}
    </div>
  );
}
