const { app, BrowserWindow, dialog, shell } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const net = require('net');
const http = require('http');

let core = null;
let win = null;
let baseUrl = '';

const isWin = process.platform === 'win32';
const binName = isWin ? 'ai-qa-report.exe' : 'ai-qa-report';

function corePath() {
  const dir = isWin ? 'win' : 'mac';
  const base = app.isPackaged ? process.resourcesPath : __dirname;
  return path.join(base, 'bin', dir, binName);
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
  });
}

function ping(port) {
  return new Promise((resolve) => {
    const req = http.get(
      { host: '127.0.0.1', port, path: '/', timeout: 800 },
      (res) => {
        res.resume();
        resolve(res.statusCode === 200);
      }
    );
    req.on('timeout', () => { req.destroy(); resolve(false); });
    req.on('error', () => resolve(false));
  });
}

async function waitReady(port, tries = 120) {
  for (let i = 0; i < tries; i++) {
    if (await ping(port)) return true;
    if (core && core.exitCode !== null) {
      throw new Error(`内核已退出（code=${core.exitCode}）`);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

async function startCore() {
  const bin = corePath();
  if (!fs.existsSync(bin)) {
    throw new Error(`找不到内核：${bin}\n请先把对应平台的 ai-qa-report 放到 desktop/bin/${isWin ? 'win' : 'mac'}/`);
  }
  if (!isWin) {
    try { fs.chmodSync(bin, 0o755); } catch (_) {}
  }
  const outDir = path.join(app.getPath('userData'), 'data');
  fs.mkdirSync(outDir, { recursive: true });

  const port = await freePort();
  core = spawn(bin, ['serve', String(port)], {
    cwd: outDir,
    windowsHide: true,
    env: { ...process.env, AI_QA_OUT: outDir },
    stdio: ['ignore', 'pipe', 'pipe']
  });
  core.stdout.on('data', (d) => console.log('[core]', d.toString().trim()));
  core.stderr.on('data', (d) => console.error('[core]', d.toString().trim()));
  core.on('exit', (code) => console.log('[core] exit', code));

  if (!(await waitReady(port))) {
    throw new Error('内核启动超时：本地服务未就绪');
  }
  baseUrl = `http://127.0.0.1:${port}`;
  return baseUrl;
}

async function createWindow() {
  await startCore();
  win = new BrowserWindow({
    width: 1280,
    height: 860,
    minWidth: 900,
    minHeight: 600,
    title: 'AI 问答记录',
    backgroundColor: '#0f1117',
    autoHideMenuBar: true,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true
    }
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith(baseUrl)) {
      win.loadURL(url);
    } else {
      shell.openExternal(url);
    }
    return { action: 'deny' };
  });

  await win.loadURL(baseUrl + '/');
  win.on('closed', () => { win = null; });
}

function stopCore() {
  if (core && core.exitCode === null) {
    try { core.kill(); } catch (_) {}
  }
  core = null;
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (win) {
      if (win.isMinimized()) win.restore();
      win.focus();
    }
  });

  app.whenReady()
    .then(createWindow)
    .catch((err) => {
      dialog.showErrorBox('启动失败', err && err.stack ? err.stack : String(err));
      app.quit();
    });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });

  app.on('window-all-closed', () => app.quit());
  app.on('before-quit', stopCore);
  app.on('will-quit', stopCore);
}
