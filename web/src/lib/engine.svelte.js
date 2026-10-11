import { host } from './api.js';
import { app, toast, refreshInfo } from './state.svelte.js';

export const busy = $state({ engine: false });

/** Runs start/stop/restart, keeping the status in sync and reporting errors. */
export async function engineAction(action) {
  if (busy.engine) return;
  busy.engine = true;
  try {
    app.status = await host[action]();
  } catch (e) {
    toast(e.message, 'error', 6000);
    refreshInfo();
  } finally {
    busy.engine = false;
  }
}
