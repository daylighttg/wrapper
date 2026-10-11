// Thin client for the GUI backend (/api/host) and the proxied agent (/api/agent).

export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

async function request(method, path, body) {
  const init = { method, headers: {} };
  if (body !== undefined) {
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
  overview: () => request('GET', '/api/agent/overview'),
  logs: (after) => request('GET', `/api/agent/logs?limit=2000${after ? `&after=${after}` : ''}`),
  login: (username, password, deviceInfo) =>
    request('POST', '/api/agent/accounts/login', { username, password, deviceInfo: deviceInfo || undefined }),
  code: (id, code) => request('POST', `/api/agent/logins/${id}/code`, { code }),
  loginStatus: (id) => request('GET', `/api/agent/logins/${id}`),
  cancelLogin: (id) => request('DELETE', `/api/agent/logins/${id}`),
  start: (id) => request('POST', `/api/agent/accounts/${id}/start`),
  stop: (id) => request('POST', `/api/agent/accounts/${id}/stop`),
  restart: (id) => request('POST', `/api/agent/accounts/${id}/restart`),
  remove: (id) => request('DELETE', `/api/agent/accounts/${id}`),
  probeHealth: () => request('POST', '/api/agent/health/probe'),
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
