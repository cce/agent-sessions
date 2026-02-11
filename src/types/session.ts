export type SessionStatus = 'waiting' | 'processing' | 'thinking' | 'compacting' | 'idle';

export type AgentType = 'claude' | 'opencode' | 'codex';

export interface Session {
  id: string;
  agentType: AgentType;
  projectName: string;
  projectPath: string;
  gitBranch: string | null;
  githubUrl: string | null;
  status: SessionStatus;
  lastMessage: string | null;
  lastMessageRole: 'user' | 'assistant' | null;
  lastActivityAt: string;
  pid: number;
  cpuUsage: number;
  activeSubagentCount: number;
  tty: string | null;
}

export interface SessionsResponse {
  sessions: Session[];
  totalCount: number;
  waitingCount: number;
}

// iTerm2 layout types for window grouping
export interface ItermSessionInfo {
  sessionId: string;
  tty: string;
  name: string;
}

export interface ItermTab {
  tabId: string;
  sessions: ItermSessionInfo[];
}

export interface ItermWindow {
  windowId: string;
  tabs: ItermTab[];
}

export interface ItermLayoutResponse {
  windows: ItermWindow[];
  sessionToWindow: Record<string, string>;
}
