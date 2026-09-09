"""Optional real-browser regression; requires agent-browser and a release binary.

Uses only synthetic credentials/data, a loopback server and a temporary database.
No credential, cookie, diary body, or browser state is printed or saved.
"""
import base64
from contextlib import closing
import hashlib
import hmac
import json
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/memento"
SESSION = "memento-test-" + secrets.token_hex(6)


def browser(*args):
    try:
        result = subprocess.run(
            ["agent-browser", "--session", SESSION, "--json", *args],
            capture_output=True, text=True, timeout=45,
        )
    except subprocess.TimeoutExpired:
        raise AssertionError("browser command timed out: " + args[0]) from None
    if result.returncode:
        raise AssertionError("browser command failed: " + args[0])
    data = json.loads(result.stdout)
    if not data["success"]:
        raise AssertionError("browser command rejected: " + args[0])
    return data["data"]


def evaluate(code):
    return browser("eval", code)["result"]


def wait(condition):
    browser("wait", "--fn", condition)


def activate(selector):
    # Native keyboard activation avoids stale off-screen mouse coordinates
    # after viewport resizing; it also checks that the controls are keyboard usable.
    browser("focus", selector)
    browser("press", "Enter")


def totp(secret):
    counter = int(time.time()) // 30
    digest = hmac.new(base64.b32decode(secret), counter.to_bytes(8, "big"), hashlib.sha1).digest()
    offset = digest[-1] & 15
    return f"{(int.from_bytes(digest[offset:offset + 4], 'big') & 0x7fffffff) % 1000000:06d}"


def main():
    if not BINARY.is_file():
        raise SystemExit("run cargo build --release first")
    password = secrets.token_urlsafe(32)
    totp_secret = base64.b32encode(secrets.token_bytes(20)).decode("ascii")
    hashed = subprocess.run(
        [str(BINARY), "hash-password", "--stdin"], input=password,
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    assert hashed.startswith(("$2a$", "$2b$", "$2y$")), "hash-password must generate bcrypt"
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    origin = "http://127.0.0.1:" + str(port)
    with tempfile.TemporaryDirectory(prefix="memento-browser-") as directory:
        settings_path = Path(directory) / "settings.yaml"
        env = {}
        env.update(MEMENTO_DB_PATH=directory + "/test.db", MEMENTO_BIND="127.0.0.1:" + str(port),
                   MEMENTO_ENV="development", MEMENTO_PASSWORD_HASH=hashed,
                   MEMENTO_TOTP_SECRET=totp_secret, MEMENTO_SESSION_TTL_DAYS="7",
                   MEMENTO_CONFIG_PATH=str(settings_path), RUST_LOG="warn")
        process = subprocess.Popen([str(BINARY)], cwd=directory, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        try:
            for _ in range(100):
                try:
                    with urllib.request.urlopen(origin + "/api/health", timeout=1):
                        break
                except OSError:
                    if process.poll() is not None:
                        raise AssertionError("temporary server exited")
                    time.sleep(0.05)
            else:
                raise AssertionError("temporary server did not start")
            browser("open", origin + "/diary")
            wait('location.pathname==="/login" && location.search==="?next=%2Fdiary" && !document.getElementById("loginForm").hidden')
            assert evaluate('(async()=>((await fetch("/api/diaries",{headers:{"X-API-Key":"not-configured"}})).status===401))()')
            wait('document.getElementById("shellAccount").textContent==="登录"')
            assert evaluate('document.getElementById("username").value==="admin" && document.getElementById("totpForm").hidden && document.getElementById("shellAccount").getAttribute("href")==="/login"')
            browser("fill", "#password", password)
            browser("click", "#loginButton")
            wait('!document.getElementById("totpForm").hidden')
            assert evaluate('document.getElementById("loginForm").hidden && document.getElementById("password").value===""')
            assert evaluate('(async()=> (await fetch("/session")).status===401 && (await fetch("/private/diaries")).status===401)()')
            assert evaluate('(()=>{const c=document.getElementById("code");return c.type==="text" && c.inputMode==="numeric" && c.autocomplete==="one-time-code" && c.maxLength===6})()')
            browser("set", "viewport", "390", "844")
            assert evaluate('document.documentElement.scrollWidth<=window.innerWidth')
            browser("fill", "#code", "000001")
            browser("click", "#backToPassword")
            assert evaluate('!document.getElementById("loginForm").hidden && document.getElementById("totpForm").hidden && document.getElementById("code").value==="" && document.getElementById("password").value===""')
            browser("fill", "#password", password)
            browser("click", "#loginButton")
            wait('!document.getElementById("totpForm").hidden')
            browser("fill", "#code", totp(totp_secret))
            browser("click", "#totpButton")
            wait('location.pathname==="/diary" && !document.getElementById("workspace").hidden && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            wait('document.getElementById("shellAccount").textContent==="管理"')
            assert evaluate('document.getElementById("shellAccount").getAttribute("href")==="/admin"')
            browser("set", "viewport", "1280", "900")
            print("PASS release assets and development password + TOTP browser login")

            browser("click", "#shellAccount")
            wait('location.pathname==="/admin" && !document.getElementById("settingsForm").hidden')
            browser("fill", "#siteName", "Synthetic Memento")
            browser("fill", "#siteSlogan", "Synthetic browser smoke")
            browser("click", "#saveSettings")
            wait('document.getElementById("adminStatus").textContent.includes("已保存")')
            assert settings_path.is_file(), "settings YAML must stay in the temporary directory"
            browser("click", ".site-brand")
            wait('location.pathname==="/" && document.querySelector(".site-brand__name").textContent==="Synthetic Memento"')
            wait('document.getElementById("shellAccount").textContent==="管理"')
            assert evaluate('document.title==="Synthetic Memento" && document.querySelector(".hero__subtitle").textContent==="Synthetic browser smoke" && document.getElementById("shellAccount").getAttribute("href")==="/admin" && document.querySelector("input[type=search]")===null')
            browser("set", "viewport", "390", "844")
            assert evaluate('(()=>{const n=document.querySelector(".site-nav"),r=n.getBoundingClientRect();return n.scrollWidth<=n.clientWidth && r.left>=0 && r.right<=innerWidth && document.documentElement.scrollWidth<=innerWidth})()')
            print("PASS isolated site settings, live home template and narrow shared navigation")

            browser("open", origin + "/diary")
            wait('!document.getElementById("workspace").hidden && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            browser("set", "viewport", "1280", "900")

            browser("fill", "#newContent", 'synthetic <img src=x onerror="window.__xss=1"> 😀')
            browser("click", '#createForm button[type="submit"]')
            wait('document.querySelectorAll("#entries article").length===1')
            assert evaluate('window.__xss===undefined && document.querySelectorAll("#entries img").length===0')
            browser("click", '#entries article button')
            browser("fill", "#editContent", "synthetic edited draft")
            # A separate client changes the resource while the editor keeps version 1.
            assert evaluate('(async()=>{const s=await (await fetch("/session")).json();return (await fetch("/private/diaries/1",{method:"PATCH",headers:{"Content-Type":"application/json","X-CSRF-Token":s.data.csrf_token,"If-Match":JSON.stringify("1")},body:JSON.stringify({content:"synthetic concurrent edit"})})).status===200})()')
            activate("#saveEdit")
            wait('!document.getElementById("conflict").hidden')
            assert evaluate('document.getElementById("editContent").value==="synthetic edited draft" && document.getElementById("saveEdit").disabled')
            activate("#refreshVersion")
            wait('!document.getElementById("latestPanel").hidden')
            activate("#acceptVersion")
            activate("#saveEdit")
            wait('document.getElementById("editor").hidden && document.getElementById("entries").textContent.includes("synthetic edited draft")')
            print("PASS create, plain-text XSS defence, conflict-preserving edit")

            # Read only this test's temporary fixture, never an operator database.
            with closing(sqlite3.connect((Path(directory) / "test.db").as_uri() + "?mode=ro", uri=True)) as fixture:
                fixture.execute("PRAGMA query_only=ON")
                before = fixture.execute(
                    "SELECT content, create_date, created_at, version, deleted_at FROM diaries WHERE id=1"
                ).fetchone()
                assert before is not None and before[4] is None
            evaluate('window.confirm=(message)=>{window.__confirmMessage=message;return true}')
            browser("click", '#entries article button:last-child')
            wait('document.querySelectorAll("#entries article").length===0')
            assert evaluate('!window.__confirmMessage.includes("永久") && window.__confirmMessage.includes("正文仍保留") && window.__confirmMessage.includes("不再显示") && window.__confirmMessage.includes("本期不提供恢复")')
            assert evaluate('!document.getElementById("notice").textContent.includes("永久") && document.getElementById("notice").textContent.includes("正文仍保留") && document.getElementById("notice").textContent.includes("本期不提供恢复")')
            assert evaluate('(async()=>{return (await fetch("/private/diaries/1")).status===404})()')
            with closing(sqlite3.connect((Path(directory) / "test.db").as_uri() + "?mode=ro", uri=True)) as fixture:
                fixture.execute("PRAGMA query_only=ON")
                after = fixture.execute(
                    "SELECT content, create_date, created_at, version, deleted_at, updated_at FROM diaries WHERE id=1"
                ).fetchone()
                assert after is not None, "soft-deleted fixture row must remain stored"
                assert after[:3] == before[:3], "soft delete must preserve content and original dates"
                assert after[3] == before[3] + 1, "soft delete must increment version"
                assert isinstance(after[4], str) and after[4], "soft delete must set deleted_at"
                assert after[4] == after[5], "deletion and update timestamps must match"
            print("PASS soft delete, retention notice and temporary fixture storage")

            browser("set", "viewport", "390", "844")
            assert evaluate('document.documentElement.scrollWidth<=window.innerWidth')
            print("PASS narrow viewport without horizontal overflow")

            evaluate('window.__realFetch=window.fetch;window.fetch=(url,opts={})=>String(url).startsWith("/private/diaries?")&&(!opts.method||opts.method==="GET")?new Promise((resolve,reject)=>opts.signal.addEventListener("abort",()=>reject(new DOMException("Aborted","AbortError")))):window.__realFetch(url,opts);true')
            browser("click", "#resetFilters")
            wait('document.getElementById("entries").getAttribute("aria-busy")==="true"')
            assert evaluate('!document.getElementById("logout").disabled')
            browser("click", "#logout")
            wait('location.pathname==="/login" && location.search==="?next=%2Fdiary" && !document.getElementById("loginForm").hidden')
            assert evaluate('(async()=>({ok:(await fetch("/session")).status===401}))()')["ok"]
            print("PASS pending-list cancellation and persisted logout")
        finally:
            try:
                browser("close")
            finally:
                process.terminate()
                logs, _ = process.communicate(timeout=10)
                assert password.encode() not in logs and hashed.encode() not in logs
                assert totp_secret.encode() not in logs
                assert b"generated ephemeral API key:" not in logs


if __name__ == "__main__":
    main()
