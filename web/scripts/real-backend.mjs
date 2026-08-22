// E2E 真后端启动器：以独立临时 data 目录与端口启动 cargo build 产物，
// 等待 /api/health 就绪后保持运行；进程退出时负责清理后端子进程与临时数据目录。
// 由 web/playwright.real-backend.mjs 的 webServer 调起。
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const webDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const projectRoot = path.resolve(webDir, '..');
const exeName = process.platform === 'win32' ? 'bulibuli.exe' : 'bulibuli';
const backendExe = path.join(projectRoot, 'target', 'debug', exeName);
const port = Number(process.env.BILI_E2E_PORT || 4188);
const healthUrl = `http://127.0.0.1:${port}/api/health`;

function log(message) {
  console.log(`[real-backend] ${message}`);
}

// 确保 debug 二进制存在（CI/本地都先 cargo build；已构建时增量编译很快）。
if (!fs.existsSync(backendExe)) {
  log('debug 二进制不存在，执行 cargo build ...');
  const build = spawnSync('cargo', ['build'], { cwd: projectRoot, stdio: 'inherit' });
  if (build.status !== 0) {
    throw new Error(`cargo build 失败，退出码 ${build.status}`);
  }
}

// 独立临时数据目录：不污染仓库内 data/。
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bulibuli-e2e-'));
log(`data dir: ${dataDir}`);

function cleanupDataDir() {
  try {
    fs.rmSync(dataDir, { recursive: true, force: true });
  } catch (error) {
    log(`清理临时数据目录失败: ${error}`);
  }
}

const child = spawn(backendExe, [], {
  cwd: projectRoot,
  env: {
    ...process.env,
    BILI__DATA_DIR: dataDir,
    BILI__PORT: String(port),
    BILI__NO_BROWSER: '1',
    BILI__SETUP_PORT_ENABLED: 'false',
  },
  stdio: ['ignore', 'pipe', 'pipe'],
});
child.stdout.on('data', chunk => process.stdout.write(chunk));
child.stderr.on('data', chunk => process.stderr.write(chunk));
child.on('exit', (code, signal) => {
  log(`backend exited code=${code} signal=${signal}`);
});

function waitForHealth(attempt = 0) {
  const request = http.get(healthUrl, response => {
    response.resume();
    if (response.statusCode === 200) {
      log(`ready at ${healthUrl}`);
      return;
    }
    retry();
  });
  request.on('error', retry);

  function retry() {
    if (child.exitCode !== null) {
      throw new Error(`后端在就绪前退出（code=${child.exitCode}），请查看上方日志`);
    }
    if (attempt > 120) {
      throw new Error(`等待 ${healthUrl} 就绪超时`);
    }
    setTimeout(() => waitForHealth(attempt + 1), 1000);
  }
}
waitForHealth();

// Playwright webServer 结束（测试完成/超时）时会向本进程发 kill 信号。
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    log(`received ${signal}, stopping backend`);
    child.kill();
    cleanupDataDir();
    process.exit(0);
  });
}
process.on('exit', () => {
  if (child.exitCode === null) child.kill();
  cleanupDataDir();
});
