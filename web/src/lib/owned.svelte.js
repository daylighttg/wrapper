// Accounts this browser added in public mode, with their logout keys
// (localStorage only; the server keeps just a hash of each key).

const KEY = 'wrapper.owned';

function load() {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || '[]');
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

export const owned = $state({ list: load() });

function save() {
  try {
    localStorage.setItem(KEY, JSON.stringify(owned.list));
  } catch {
    /* storage unavailable: the key is still shown once */
  }
}

/** Remembers (or replaces) an account: {ref, name, username, key, at}. */
export function remember(entry) {
  owned.list = [...owned.list.filter((o) => o.username !== entry.username), entry];
  save();
}

export function forget(username) {
  owned.list = owned.list.filter((o) => o.username !== username);
  save();
}
