#!/usr/bin/env node
// Extract the TeleAgent desktop login token (`<tenant>:<JWT>`) and device
// identity so they can be pasted into the PolyGate admin console.
//
// Sources (all under the client's own data root):
//   Local State                                  os_crypt.encrypted_key (DPAPI)
//   app-auth/device-meta.json                    deviceId / installId
//   users/v1_public_<id>/app-auth/token.json      v10-encrypted login token
//
// Nothing is synthesized: a missing field is reported, never guessed.

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');

function fail(message) {
  console.error(`\n[x] ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const args = { root: null, out: null, print: false, user: null };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--root') args.root = argv[++i];
    else if (flag === '--out') args.out = argv[++i];
    else if (flag === '--user') args.user = argv[++i];
    else if (flag === '--print') args.print = true;
    else if (flag === '-h' || flag === '--help') {
      console.log('用法: node teleagent-extract.js [--root "<TeleAgent 数据目录>"] [--user v1_public_xxx] [--out <输出文件>] [--print]');
      process.exit(0);
    }
  }
  return args;
}

function defaultRoot() {
  return path.join(os.homedir(), '.local', 'share', 'TeleAgent');
}

// Chromium wraps the AES key with DPAPI; only the same Windows user can unwrap it.
function osCryptKey(localStatePath) {
  if (!fs.existsSync(localStatePath)) {
    fail(`找不到 ${localStatePath}：TeleAgent 桌面端还没有登录过。`);
  }
  const state = JSON.parse(fs.readFileSync(localStatePath, 'utf8'));
  const osCrypt = state.os_crypt || {};
  if (!osCrypt.encrypted_key) {
    fail('Local State 里没有 os_crypt.encrypted_key，这个客户端可能没有启用加密存储。');
  }
  const wrapped = Buffer.from(osCrypt.encrypted_key, 'base64');
  if (wrapped.subarray(0, 5).toString('latin1') !== 'DPAPI') {
    fail('os_crypt.encrypted_key 不是 DPAPI 格式，当前只支持 Windows DPAPI。');
  }
  const blob = wrapped.subarray(5).toString('base64');
  const script =
    'Add-Type -AssemblyName System.Security; ' +
    `$b=[Convert]::FromBase64String('${blob}'); ` +
    "$p=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,'CurrentUser'); " +
    '[Convert]::ToBase64String($p)';
  let out;
  try {
    out = execFileSync('powershell', ['-NoProfile', '-NonInteractive', '-Command', script], {
      encoding: 'utf8',
      windowsHide: true,
    });
  } catch (error) {
    fail(`DPAPI 解包失败（需要与 TeleAgent 同一个 Windows 账号）: ${String(error.stderr || error.message).trim()}`);
  }
  const key = Buffer.from(out.trim(), 'base64');
  if (key.length !== 32) fail(`存储密钥长度异常: ${key.length}`);
  return key;
}

// v10 = "v10" + nonce(12) + ciphertext + tag(16); AES-256-GCM keyed by DPAPI.
function decryptV10(key, encoded) {
  const buf = Buffer.from(encoded, 'base64');
  const version = buf.subarray(0, 3).toString('latin1');
  if (version !== 'v10') fail(`不支持的密文版本 "${version}"（只支持 v10）。`);
  const nonce = buf.subarray(3, 15);
  const tag = buf.subarray(buf.length - 16);
  const body = buf.subarray(15, buf.length - 16);
  const decipher = crypto.createDecipheriv('aes-256-gcm', key, nonce);
  decipher.setAuthTag(tag);
  try {
    return Buffer.concat([decipher.update(body), decipher.final()]).toString('utf8');
  } catch {
    fail('AES-GCM 解密失败：密文可能被截断，或属于另一个 Windows 用户。');
  }
}

function listUsers(root) {
  const dir = path.join(root, 'users');
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name.startsWith('v1_public_'))
    .map((entry) => {
      const tokenPath = path.join(dir, entry.name, 'app-auth', 'token.json');
      let mtime = 0;
      try {
        mtime = fs.statSync(tokenPath).mtimeMs;
      } catch {
        // No token for this user yet.
      }
      return { name: entry.name, tokenPath, mtime };
    })
    .filter((user) => user.mtime > 0)
    .sort((a, b) => b.mtime - a.mtime);
}

function readJson(file, what) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (error) {
    fail(`读取 ${what} 失败 (${file}): ${error.message}`);
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const root = args.root || defaultRoot();
  if (!fs.existsSync(root)) {
    fail(`找不到 TeleAgent 数据目录 ${root}；如果装在其他位置，用 --root 指定。`);
  }
  const users = listUsers(root);
  const user = args.user ? users.find((item) => item.name === args.user) : users[0];
  if (!user) {
    fail('没有找到任何已登录的 TeleAgent 账号（users/v1_public_*/app-auth/token.json）。');
  }

  const key = osCryptKey(path.join(root, 'Local State'));
  const tokenFile = readJson(user.tokenPath, 'token.json');
  if (!tokenFile.encryptedToken) {
    fail('token.json 里没有 encryptedToken；这个客户端版本可能换了存储格式。');
  }
  const token = decryptV10(key, tokenFile.encryptedToken).trim();
  if (!token.includes(':') || token.split('.').length < 3) {
    fail(`解密结果不像 <租户>:<JWT>: ${token.slice(0, 24)}…`);
  }

  const metaPath = path.join(root, 'app-auth', 'device-meta.json');
  let deviceMeta = {};
  if (fs.existsSync(metaPath)) {
    deviceMeta = readJson(metaPath, 'device-meta.json');
  }

  const payload = JSON.parse(
    Buffer.from(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8'),
  );
  const result = {
    token,
    device_id: deviceMeta.deviceId || '',
    install_id: deviceMeta.installId || '',
    app_version: '',
    user: user.name,
    tenant: String(token.split(':')[0] || ''),
    account_name: payload.accountName || payload.userName || '',
    expires_at: payload.exp || null,
  };

  if (args.print) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    const target = args.out || 'teleagent-account.json';
    fs.writeFileSync(target, `${JSON.stringify(result, null, 2)}\n`);
    console.log(`[ok] 已写入 ${path.resolve(target)}`);
    console.log(`     账号: ${result.account_name || user.name} · 租户 ${result.tenant}`);
    if (result.expires_at) {
      console.log(`     令牌到期: ${new Date(result.expires_at * 1000).toLocaleString('zh-CN', { hour12: false })}`);
    }
    console.log('     把这个文件里的 token / device_id / install_id 填进 PolyGate 管理台「添加账号 → TeleAgent」。');
  }
}

main();
