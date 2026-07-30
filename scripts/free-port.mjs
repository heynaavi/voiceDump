// Free the dev port before Vite binds it, so a stale `tauri dev` (or a crashed
// one that didn't release the socket) never blocks a fresh start with
// "Port 1420 is already in use". Vite's devUrl is pinned to 1420 in
// tauri.conf.json, so auto-incrementing to another port would break Tauri —
// clearing the port is the correct fix, not moving it.
//
// macOS/Linux use `lsof`; Windows uses `netstat`. Anything unexpected is
// swallowed: the worst case is Vite reporting the in-use port itself, i.e. the
// old behaviour, never a hard crash of this helper.
import { execSync } from 'node:child_process';

const port = process.argv[2] || '1420';

function pidsOnPort(p) {
  try {
    if (process.platform === 'win32') {
      const out = execSync(`netstat -ano -p tcp | findstr :${p}`, { stdio: ['ignore', 'pipe', 'ignore'] }).toString();
      return [...new Set(out.trim().split('\n').map((l) => l.trim().split(/\s+/).pop()).filter(Boolean))];
    }
    const out = execSync(`lsof -ti tcp:${p} -sTCP:LISTEN`, { stdio: ['ignore', 'pipe', 'ignore'] }).toString();
    return out.trim().split('\n').filter(Boolean);
  } catch {
    return []; // nothing listening — the good case
  }
}

const pids = pidsOnPort(port);
if (pids.length === 0) {
  process.exit(0);
}

try {
  const signal = process.platform === 'win32' ? '/F /PID' : '';
  for (const pid of pids) {
    if (process.platform === 'win32') execSync(`taskkill /F /PID ${pid}`, { stdio: 'ignore' });
    else execSync(`kill ${pid}`, { stdio: 'ignore' });
  }
  console.log(`[dev] port ${port} was busy — freed it (stopped stale process ${pids.join(', ')})`);
} catch (err) {
  console.warn(`[dev] port ${port} is busy and couldn't be freed automatically (${err.message}). Vite may error next.`);
}
