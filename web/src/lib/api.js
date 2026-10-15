// Thin client for the front door: the GUI backend (/api/host), the agent's
// management API (/api, proxied with its token by the GUI or served by a
// headless agent) and public mode (/api/public).

export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

async function request(method, path, body, raw = false) {
  const init = { method, headers: {} };
  if (raw) {
    init.body = body;
  } else if (body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let res;
  try {
    res = await fetch(path, init);
  } catch {
    throw new ApiError('network error: is wrapper still running?', 0);
  }
  let json = null;
  try {
    json = await res.json();
  } catch {
    throw new ApiError(`HTTP ${res.status}`, res.status);
  }
  if (!json || json.code !== 0) {
    throw new ApiError(json?.msg || `HTTP ${res.status}`, res.status);
  }
  return json.data;
}

export const session = {
  whoami: () => request('GET', '/api/whoami'),
  signIn: (token) => request('POST', '/api/session', { token }),
  signOut: () => request('DELETE', '/api/session'),
};

export const host = {
  info: () => request('GET', '/api/host/info'),
  config: () => request('GET', '/api/host/config'),
  saveConfig: (cfg) => request('PUT', '/api/host/config', cfg),
  status: () => request('GET', '/api/host/status'),
  start: () => request('POST', '/api/host/start'),
  stop: () => request('POST', '/api/host/stop'),
  restart: () => request('POST', '/api/host/restart'),
  console: (after) => request('GET', `/api/host/console?limit=2000${after ? `&after=${after}` : ''}`),
  download: () => request('POST', '/api/host/package/download'),
};

export const agent = {
  overview: () => request('GET', '/api/overview'),
  logs: (after) => request('GET', `/api/logs?limit=2000${after ? `&after=${after}` : ''}`),
  login: (username, password, deviceInfo) =>
    request('POST', '/api/accounts/login', { username, password, deviceInfo: deviceInfo || undefined }),
  code: (id, code) => request('POST', `/api/logins/${id}/code`, { code }),
  loginStatus: (id) => request('GET', `/api/logins/${id}`),
  cancelLogin: (id) => request('DELETE', `/api/logins/${id}`),
  start: (id) => request('POST', `/api/accounts/${id}/start`),
  stop: (id) => request('POST', `/api/accounts/${id}/stop`),
  restart: (id) => request('POST', `/api/accounts/${id}/restart`),
  remove: (id) => request('DELETE', `/api/accounts/${id}`),
  probeHealth: () => request('POST', '/api/health/probe'),
  stats: () => request('GET', '/api/stats'),
  settings: () => request('GET', '/api/settings'),
  saveSettings: (patch) => request('PUT', '/api/settings', patch),
  restore: (file, mode, settings) =>
    request('POST', `/api/restore?mode=${mode}&settings=${settings ? 1 : 0}`, file, true),
  pull: (url, token, mode, settings) => request('POST', '/api/restore/pull', { url, token, mode, settings }),
};

export const pub = {
  overview: () => request('GET', '/api/public/overview'),
  login: (username, password) => request('POST', '/api/public/login', { username, password }),
  status: (ticket) => request('GET', `/api/public/login/${ticket}`),
  code: (ticket, code) => request('POST', `/api/public/login/${ticket}/code`, { code }),
  cancel: (ticket) => request('DELETE', `/api/public/login/${ticket}`),
  logout: (username, key) => request('POST', '/api/public/logout', { username, key }),
};

/** EventSource that reconnects with backoff; returns a close function. */
export function subscribe(url, onEvent, onState) {
  let es = null;
  let closed = false;
  let delay = 1000;
  let timer = null;
  const open = () => {
    if (closed) return;
    es = new EventSource(url);
    es.onopen = () => {
      delay = 1000;
      onState?.(true);
    };
    es.onmessage = (m) => {
      try {
        onEvent(JSON.parse(m.data));
      } catch {
        /* ignore malformed events */
      }
    };
    es.onerror = () => {
      onState?.(false);
      es.close();
      timer = setTimeout(open, delay);
      delay = Math.min(delay * 2, 15000);
    };
  };
  open();
  return () => {
    closed = true;
    clearTimeout(timer);
    es?.close();
  };
}
