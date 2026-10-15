// Helpers for the request counters (`stats.json` on the agent).

export const ENDPOINTS = ['m3u8', 'key', 'lyrics', 'webplayback', 'license'];

/** Requests in an endpoint -> {ok, fail} map (optionally one endpoint). */
export function total(counts, endpoint) {
  if (!counts) return 0;
  const list = endpoint ? [counts[endpoint]] : Object.values(counts);
  return list.reduce((n, c) => n + (c ? c.ok + c.fail : 0), 0);
}

export function okCount(counts, endpoint) {
  if (!counts) return 0;
  const list = endpoint ? [counts[endpoint]] : Object.values(counts);
  return list.reduce((n, c) => n + (c ? c.ok : 0), 0);
}

/** Success share in percent, or null without requests. */
export function okRate(counts, endpoint) {
  const n = total(counts, endpoint);
  return n ? Math.round((okCount(counts, endpoint) / n) * 1000) / 10 : null;
}

export const fmt = (n) => Number(n || 0).toLocaleString();

/** Day number (days since 1970-01-01 UTC) -> short date. */
export function dayLabel(day, opts = { month: 'short', day: 'numeric' }) {
  return new Date(day * 86400000).toLocaleDateString(undefined, { ...opts, timeZone: 'UTC' });
}
