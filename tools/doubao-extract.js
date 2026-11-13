#!/usr/bin/env node
// Extract the Doubao desktop login (cookies + device identity) from its
// Chromium profile so it can be pasted into the PolyGate admin console.
//
// Nothing is synthesized here: every value comes from the profile on disk.
// If a field cannot be found, it is reported as missing instead of guessed.

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');

const APP_AID = '582478'; // www.doubao.com web app id
const COOKIE_HOSTS = ['.doubao.com', 'www.doubao.com'];

function fail(message) {
  console.error(`\n[x] ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const args = { dir: null, profile: null, out: null, kill: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--dir') args.dir = argv[++i];
    else if (flag === '--profile') args.profile = argv[++i];
    else if (flag === '--out') args.out = argv[++i];
    else if (flag === '--kill') args.kill = true;
    else if (flag === '--print') args.print = true;
    else if (flag === '-h' || flag === '--help') {
      console.log('用法: node doubao-extract.js [--dir "<User Data 目录>"] [--profile Default] [--out <输出文件>] [--kill] [--print]');
      process.exit(0);
    }
  }
  return args;
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

// The active profile's cookie DB is opened without sharing, so it can only be
// read once the client lets go of it.
function closeClient() {
  try {
    execFileSync('taskkill', ['/F', '/IM', 'Doubao.exe'], { stdio: 'ignore', windowsHide: true });
  } catch {
    // Not running, or already gone.
  }
  sleep(2500);
}

function copyDatabase(src, dst, options) {
  for (let attempt = 0; ; attempt += 1) {
    try {
      fs.copyFileSync(src, dst);
      return true;
    } catch (error) {
      if (error.code !== 'EBUSY') throw error;
      if (attempt === 0 && options.kill) {
        console.log('Cookies 库被占用，正在关闭豆包…');
        closeClient();
        continue;
      }
      if (attempt >= 2) return false;
      sleep(600);
    }
  }
}

function locateUserData(explicit) {
  const roots = explicit
    ? [explicit]
    : [
        path.join(process.env.LOCALAPPDATA || '', 'Doubao', 'User Data'),
        path.join(process.env.LOCALAPPDATA || '', 'DoubaoWork', 'User Data'),
      ];
  for (const root of roots) {
    if (root && fs.existsSync(path.join(root, 'Local State'))) return root;
  }
  fail(`找不到豆包用户数据目录。默认位置: ${roots[0]}\n    请确认豆包桌面端已登录过，或用 --dir 指定。`);
}

function listProfiles(userData, wanted) {
  const profiles = [];
  for (const entry of fs.readdirSync(userData, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    if (entry.name !== 'Default' && !/^Profile \d+$/.test(entry.name)) continue;
    const dbPath = path.join(userData, entry.name, 'Network', 'Cookies');
    if (!fs.existsSync(dbPath)) continue;
    profiles.push({ name: entry.name, dbPath, at: fs.statSync(dbPath).mtimeMs });
  }
  if (wanted) {
    const picked = profiles.filter((p) => p.name === wanted);
    if (!picked.length) fail(`档案 ${wanted} 不存在，可选: ${profiles.map((p) => p.name).join(', ')}`);
    return picked;
  }
  // The account last used in the client has the freshest cookie DB.
  return profiles.sort((a, b) => b.at - a.at);
}

// Chromium wraps the AES key with DPAPI; only DPAPI can unwrap it.
function osCryptKey(localStatePath) {
  const state = JSON.parse(fs.readFileSync(localStatePath, 'utf8'));
  const osCrypt = state.os_crypt || {};
  if (!osCrypt.encrypted_key) {
    fail('Local State 里没有 os_crypt.encrypted_key，这个客户端可能没有启用加密 Cookie。');
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
    fail(`DPAPI 解包失败（需要与豆包同一个 Windows 账号）: ${String(error.stderr || error.message).trim()}`);
  }
  const key = Buffer.from(out.trim(), 'base64');
  if (key.length !== 32) fail(`Cookie 密钥长度异常: ${key.length}`);
  return key;
}

// v10 = "v10" + nonce(12) + ciphertext + tag(16); AES-256-GCM keyed by DPAPI.
// Chromium prepends SHA256(host_key) to the plaintext since M80.
function decryptCookie(key, hostKey, raw) {
  const buf = Buffer.from(raw);
  const version = buf.subarray(0, 3).toString('latin1');
  if (version === 'v20') {
    fail('Cookie 使用 app-bound 加密（v20），本脚本不支持；请更新客户端后再试。');
  }
  if (version !== 'v10') return null;
  const nonce = buf.subarray(3, 15);
  const tag = buf.subarray(buf.length - 16);
  const body = buf.subarray(15, buf.length - 16);
  const decipher = crypto.createDecipheriv('aes-256-gcm', key, nonce);
  decipher.setAuthTag(tag);
  let plain;
  try {
    plain = Buffer.concat([decipher.update(body), decipher.final()]);
  } catch {
    return null;
  }
  const prefix = crypto.createHash('sha256').update(hostKey).digest();
  if (plain.length > 32 && plain.subarray(0, 32).equals(prefix)) plain = plain.subarray(32);
  return plain.toString('utf8');
}

function readCookies(dbPath, key, options) {
  const { DatabaseSync } = require('node:sqlite');
  const copy = path.join(os.tmpdir(), `polygate-doubao-cookies-${process.pid}.db`);
  if (!copyDatabase(dbPath, copy, options)) return null;
  let rows;
  try {
    const db = new DatabaseSync(copy);
    rows = db.prepare('select host_key, name, encrypted_value, value from cookies').all();
    db.close();
  } finally {
    fs.rmSync(copy, { force: true });
  }
  const jars = new Map();
  for (const row of rows) {
    if (!COOKIE_HOSTS.includes(row.host_key)) continue;
    let value = row.value;
    if ((!value || value.length === 0) && row.encrypted_value && row.encrypted_value.length) {
      value = decryptCookie(key, row.host_key, row.encrypted_value);
    }
    if (value === null || value === undefined) continue;
    if (!jars.has(row.host_key)) jars.set(row.host_key, new Map());
    jars.get(row.host_key).set(row.name, value);
  }
  // A host-only cookie on www.doubao.com shadows the .doubao.com one.
  const merged = new Map();
  for (const host of COOKIE_HOSTS) {
    const jar = jars.get(host);
    if (!jar) continue;
    for (const [name, value] of jar) merged.set(name, value);
  }
  return merged;
}

function leveldbText(profileDir) {
  const dir = path.join(profileDir, 'Local Storage', 'leveldb');
  if (!fs.existsSync(dir)) return [];
  const chunks = [];
  for (const name of fs.readdirSync(dir)) {
    if (!/\.(ldb|log)$/.test(name)) continue;
    try {
      chunks.push(fs.readFileSync(path.join(dir, name)).toString('latin1'));
    } catch {
      // A live leveldb may rotate files under us; the rest is still useful.
    }
  }
  return chunks;
}

// The web app keeps its identity in localStorage under tea_cache_tokens_<aid>.
function findWebId(chunks, deviceId) {
  const records = [];
  for (const text of chunks) {
    const keyRe = /tea_cache_tokens_(\d{1,10})/g;
    let match;
    while ((match = keyRe.exec(text))) {
      const tail = text.slice(match.index, match.index + 900);
      const web = /web_id":"(\d{10,25})/.exec(tail);
      if (!web) continue;
      const user = /user_unique_\w{0,3}"?(\d{10,25})/.exec(tail);
      const stamp = /timestamp":(\d{10,16})/.exec(tail);
      records.push({
        aid: match[1],
        webId: web[1],
        userId: user ? user[1] : '',
        at: stamp ? Number(stamp[1]) : 0,
      });
    }
  }
  const byAid = records.filter((r) => r.aid === APP_AID).sort((a, b) => b.at - a.at);
  if (byAid.length) return byAid[0].webId;
  if (deviceId) {
    const byDevice = records.filter((r) => r.userId === deviceId).sort((a, b) => b.at - a.at);
    if (byDevice.length) return byDevice[0].webId;
  }
  const anyWeb = /web_id":"(\d{10,25})/.exec(chunks.join(''));
  return anyWeb ? anyWeb[1] : '';
}

// msToken's s_v_web_id is what the page sends as `fp`.
function findLocalStorageValue(chunks, key, valueRe) {
  for (const text of chunks) {
    let from = 0;
    for (;;) {
      const at = text.indexOf(key, from);
      if (at < 0) break;
      const tail = text.slice(at, at + 400);
      const match = valueRe.exec(tail);
      if (match) return match[1];
      from = at + key.length;
    }
  }
  return '';
}

function mask(value, keep = 4) {
  if (!value) return '(空)';
  if (value.length <= keep * 2) return `${value.slice(0, keep)}…`;
  return `${value.slice(0, keep)}…${value.slice(-keep)}`;
}

function main() {
  const major = Number(process.versions.node.split('.')[0]);
  if (major < 22) fail(`需要 Node.js 22.5 以上（当前 ${process.versions.node}），node:sqlite 不可用。`);
  const args = parseArgs(process.argv.slice(2));
  const userData = locateUserData(args.dir);
  const localStatePath = path.join(userData, 'Local State');
  const state = JSON.parse(fs.readFileSync(localStatePath, 'utf8'));
  const aha = (state.aha && state.aha.device) || {};
  const deviceId = String(aha.device_id || '');
  const installId = String(aha.install_id || '');
  const key = osCryptKey(localStatePath);

  console.log(`用户数据目录: ${userData}`);
  const profiles = listProfiles(userData, args.profile);
  const newest = args.profile ? '' : profiles[0].name;
  const accounts = [];
  const locked = [];
  for (const profile of profiles) {
    const profileDir = path.join(userData, profile.name);
    const jar = readCookies(profile.dbPath, key, args);
    if (!jar) {
      locked.push(profile.name);
      continue;
    }
    if (!jar.has('sessionid') && !jar.has('sid_guard')) continue;

    const chunks = leveldbText(profileDir);
    const webId = findWebId(chunks, deviceId);
    const fpLocal = findLocalStorageValue(chunks, 's_v_web_id', /(verify_[0-9A-Za-z]{6,}(?:_[0-9A-Za-z]{2,}){2,8})/);
    const fpCookie = jar.get('s_v_web_id') || '';
    const cookies = [...jar.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([name, value]) => `${name}=${value}`)
      .join('; ');

    accounts.push({
      profile: profile.name,
      active: profile.name === newest,
      cookies,
      device_id: deviceId,
      web_id: webId,
      fp: fpLocal || fpCookie,
      fp_source: fpLocal ? 'localStorage' : fpCookie ? 'cookie' : '',
      ms_token: jar.get('msToken') || null,
      name: `Doubao ${profile.name === 'Default' ? '' : `${profile.name} `}${deviceId.slice(-6)}`.trim(),
      install_id: installId,
      cookie_count: jar.size,
    });
  }

  // Never fall back to an older account silently: that would hand the operator
  // the wrong login instead of the one they just switched to.
  if (locked.includes(newest)) {
    fail(
      `当前在用的档案 ${newest} 正被豆包独占，读不到 Cookie 库。\n` +
        '    请先在托盘退出豆包再重跑；或者加 --kill 让脚本替你关掉它。',
    );
  }

  if (!accounts.length) {
    if (locked.length) {
      fail(
        `档案 ${locked.join('、')} 正被豆包独占，读不到 Cookie 库。\n` +
          '    请先在托盘退出豆包再重跑；或者加 --kill 让脚本替你关掉它。',
      );
    }
    fail('没有找到已登录的豆包档案（缺少 sessionid Cookie）。请先在豆包桌面端登录。');
  }

  const outPath = args.out || path.join(__dirname, 'doubao-account.json');
  const payload = accounts.length === 1 ? accounts[0] : { accounts };
  fs.writeFileSync(outPath, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');

  for (const account of accounts) {
    console.log('');
    console.log(`档案 ${account.profile}${account.active ? '（最近使用）' : ''}: ${account.cookie_count} 个 Cookie`);
    console.log(`  device_id: ${account.device_id || '(缺失)'}`);
    console.log(`  web_id   : ${account.web_id || '(缺失)'}`);
    console.log(`  fp       : ${mask(account.fp, 18)}  (${account.fp_source || '缺失'})`);
    const missing = ['cookies', 'device_id', 'web_id', 'fp'].filter((field) =>
      field === 'cookies' ? !account.cookies.includes('sessionid') : !account[field],
    );
    if (missing.length) {
      console.log(`  [!] 缺失字段: ${missing.join(', ')} —— 管理台不会替你生成，需要从客户端请求参数里补。`);
    }
    if (args.print) {
      console.log('  ---- 直接粘贴用（含明文凭据）----');
      console.log(`  Cookie   : ${account.cookies}`);
      console.log(`  device_id: ${account.device_id}`);
      console.log(`  web_id   : ${account.web_id}`);
      console.log(`  fp       : ${account.fp}`);
    }
  }
  console.log(`\n[ok] 已写入 ${outPath}`);
  if (locked.length) {
    console.log(`[!] 档案 ${locked.join('、')} 被豆包独占没读到；退出豆包或加 --kill 可补上。`);
  }
  if (accounts.length > 1) console.log('     里面有多个档案，第一个是最近使用的那个。');
  console.log('     管理台 → Doubao → 导入登录态，把上面四项分别粘进去即可。');
}

main();
