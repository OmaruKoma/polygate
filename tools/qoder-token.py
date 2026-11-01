"""Copy the Qoder CN desktop access token to the clipboard.

Reads the Electron safeStorage blob (auth.v1.dat) from the Qoder CN data
directory, unwraps the os_crypt key with DPAPI, and decrypts the v10 AES-GCM
payload. The token itself is never printed; only its length and expiry are.

Usage: python D:\\tools\\qoder-token.py
"""

import base64
import ctypes
import json
import pathlib
import subprocess
import sys
from ctypes import wintypes

APPDATA = pathlib.Path.home() / "AppData/Roaming"
CANDIDATES = [
    APPDATA / "com.qodercn.app.stable",  # Qoder CN (China edition)
    APPDATA / "Qoder",                    # Qoder (international edition)
]


class DATA_BLOB(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def dpapi_unprotect(blob: bytes) -> bytes:
    crypt32 = ctypes.windll.crypt32
    kernel32 = ctypes.windll.kernel32
    buf = ctypes.create_string_buffer(blob, len(blob))
    data_in = DATA_BLOB(len(blob), ctypes.cast(buf, ctypes.POINTER(ctypes.c_char)))
    data_out = DATA_BLOB()
    if not crypt32.CryptUnprotectData(
        ctypes.byref(data_in), None, None, None, None, 0, ctypes.byref(data_out)
    ):
        raise OSError("CryptUnprotectData failed")
    try:
        return ctypes.string_at(data_out.pbData, data_out.cbData)
    finally:
        kernel32.LocalFree(data_out.pbData)


def decrypt(root: pathlib.Path) -> dict:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    state = json.loads((root / "Local State").read_text(encoding="utf-8"))
    wrapped = base64.b64decode(state["os_crypt"]["encrypted_key"])
    if wrapped[:5] != b"DPAPI":
        raise SystemExit("unexpected os_crypt wrapper (not DPAPI)")
    key = dpapi_unprotect(wrapped[5:])

    raw = (root / "auth.v1.dat").read_bytes()
    if raw[:3] == b"v10":
        plain = AESGCM(key).decrypt(raw[3:15], raw[15:], None)
    elif raw[:3] == b"v11":
        raise SystemExit("Chrome v11 (app-bound) keys are not supported here")
    else:
        plain = raw
    return json.loads(plain)


def main() -> int:
    for root in CANDIDATES:
        if (root / "auth.v1.dat").is_file() and (root / "Local State").is_file():
            break
    else:
        print("No Qoder desktop login found; sign in to the Qoder app first.", file=sys.stderr)
        return 1

    auth = decrypt(root)
    token = auth.get("token") or ""
    if not token:
        print(f"{root.name} has no access token; log in again in the app.", file=sys.stderr)
        return 1

    subprocess.run("clip", input=token.encode(), check=True)
    user = auth.get("user") or {}
    print(f"source : {root}")
    print(f"account: {user.get('id', '?')} {user.get('name', '')}".rstrip())
    print(f"token  : {token[:3]}... ({len(token)} chars) copied to clipboard")
    print(f"expires: {auth.get('expiresAt', 'unknown')}")
    print("Paste it into PolyGate -> Suanlidou -> 导入令牌. Do not post it anywhere.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
