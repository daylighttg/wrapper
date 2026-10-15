// Global reactive state, fed by the two SSE streams.
import { host, agent, subscribe } from './api.js';

const MAX_LINES = 4000;

export const app = $state({
  info: null,
  config: null,
  status: null,
  overview: null,
  accounts: [],
  logins: {},
  logs: [],
  console: [],
  download: null,
  hostConnected: false,
  agentConnected: false,
  toasts: [],
});

let toastId = 0;
export function toast(message, kind = 'info', ms = 4000) {
  const id = ++toastId;
  app.toasts.push({ id, message, kind });
  setTimeout(() => {
    app.toasts = app.toasts.filter((t) => t.id !== id);
  }, ms);
}

function pushBounded(list, item) {
  list.push(item);
  if (list.length > MAX_LINES) list.splice(0, list.length - MAX_LINES);
}

function upsertAccount(acc) {
  const i = app.accounts.findIndex((a) => a.id === acc.id);
  if (i >= 0) app.accounts[i] = acc;
  else app.accounts.push(acc);
}

export const engineUp = () => ['running', 'starting'].includes(app.status?.state);

export async function refreshInfo() {
  try {
    app.info = await host.info();
  } catch {
    /* host offline: the SSE state shows it */
  }
}

export async function refreshAgent() {
  try {
    const ov = await agent.overview();
    app.overview = ov;
    app.accounts = ov.accounts;
    app.logins = Object.fromEntries(ov.logins.map((l) => [l.loginId, l]));
  } catch {
    app.overview = null;
  }
}

let closeAgent = null;
let lastLogSeq = 0;

async function connectAgent() {
  if (closeAgent) return;
  try {
    const logs = await agent.logs(lastLogSeq || undefined);
    for (const l of logs) pushBounded(app.logs, l);
    if (logs.length) lastLogSeq = logs[logs.length - 1].seq;
  } catch {
    /* agent not reachable yet; SSE retries */
  }
  await refreshAgent();
  closeAgent = subscribe(
    '/api/agent/events',
    (ev) => {
      switch (ev.type) {
        case 'log':
          if (ev.seq > lastLogSeq) {
            lastLogSeq = ev.seq;
            pushBounded(app.logs, ev);
          }
          break;
        case 'account':
          upsertAccount(ev.account);
          break;
        case 'accountRemoved':
          app.accounts = app.accounts.filter((a) => a.id !== ev.id);
          break;
        case 'login':
          app.logins[ev.login.loginId] = ev.login;
          break;
      }
    },
    (up) => {
      const was = app.agentConnected;
      app.agentConnected = up;
      if (up && !was) refreshAgent();
    },
  );
}

function disconnectAgent() {
  closeAgent?.();
  closeAgent = null;
  app.agentConnected = false;
  app.overview = null;
  // A new agent process restarts its log sequence.
  lastLogSeq = 0;
}

function onStatus(status) {
  const prev = app.status?.state;
  app.status = status;
  if (['running', 'starting'].includes(status.state)) connectAgent();
  else if (prev && prev !== status.state) disconnectAgent();
}

export async function init() {
  try {
    const [info, config, status, consoleLines] = await Promise.all([
      host.info(),
      host.config(),
      host.status(),
      host.console(),
    ]);
    app.info = info;
    app.config = config;
    app.console = consoleLines;
    onStatus(status);
  } catch (e) {
    toast(e.message, 'error');
  }
  subscribe(
    '/api/host/events',
    (ev) => {
      if (ev.type === 'console') pushBounded(app.console, ev);
      else if (ev.type === 'status') onStatus(ev.status);
      else if (ev.type === 'download') {
        app.download = ev.progress;
        if (ev.progress.status === 'done' || ev.progress.status === 'error') refreshInfo();
      } else if (ev.type === 'update') refreshInfo();
    },
    async (up) => {
      const was = app.hostConnected;
      app.hostConnected = up;
      if (up && !was) {
        try {
          onStatus(await host.status());
        } catch {
          /* ignore */
        }
      }
    },
  );
}
