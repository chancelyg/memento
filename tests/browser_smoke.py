"""Optional real-browser regression; requires agent-browser and a release binary.

Uses only synthetic credentials/data, a loopback server and a temporary database.
No credential, cookie, diary body, or browser state is printed or exported.
Draft storage is exercised only in this test's isolated browser session.
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
NEW_KEY = "memento-diary-new-v1"
EDIT_KEY = "memento-diary-edit-v1"
READY = '!document.getElementById("workspace").hidden && document.getElementById("entries").getAttribute("aria-busy")==="false"'


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


def open_editor(selector="#entries article button"):
    # Editor/list layout changes can invalidate mouse coordinates; wait for the
    # actionable list and use keyboard activation before inspecting the editor.
    wait(READY)
    wait('!document.querySelector(%s).disabled' % json.dumps(selector))
    activate(selector)
    wait('!document.getElementById("editor").hidden')


def totp(secret, counter):
    digest = hmac.new(base64.b32decode(secret), counter.to_bytes(8, "big"), hashlib.sha1).digest()
    offset = digest[-1] & 15
    return f"{(int.from_bytes(digest[offset:offset + 4], 'big') & 0x7fffffff) % 1000000:06d}"


def finish_login(secret, previous_step=-1):
    # Wait for a genuinely newer time step; never reset server replay protection.
    while int(time.time()) // 30 <= previous_step:
        time.sleep(0.2)
    step = int(time.time()) // 30
    browser("fill", "#code", totp(secret, step))
    browser("click", "#totpButton")
    wait('location.pathname!=="/login"')
    return step


def login(password, secret, previous_step):
    wait('!document.getElementById("loginForm").hidden')
    browser("fill", "#password", password)
    browser("click", "#loginButton")
    wait('!document.getElementById("totpForm").hidden')
    return finish_login(secret, previous_step)


def login_layout(second_step=False):
    for width, height in ((1280, 900), (390, 844)):
        browser("set", "viewport", str(width), str(height))
        assert evaluate('document.documentElement.scrollWidth<=innerWidth'), "login viewport overflow"
        if not second_step:
            assert evaluate('''(()=>{
                const input=document.getElementById("password").getBoundingClientRect();
                const button=document.getElementById("loginButton").getBoundingClientRect();
                return input.width>0 && Math.abs(input.width-button.width)<=1 &&
                    Math.abs(input.left-button.left)<=1 && Math.abs(input.right-button.right)<=1 &&
                    button.top-input.bottom>=16 && button.left>=0 && button.right<=innerWidth;
            })()'''), "password and login button must align with at least 16px separation"
        else:
            assert evaluate('''(()=>{
                const input=document.getElementById("code").getBoundingClientRect();
                const buttons=["backToPassword","totpButton"].map(id=>document.getElementById(id));
                const rects=buttons.map(button=>button.getBoundingClientRect());
                const [a,b]=rects;
                return rects.every((r,i)=>r.width>0 && r.left>=input.left-1 && r.right<=input.right+1 &&
                    r.top-input.bottom>=16 && buttons[i].scrollWidth<=buttons[i].clientWidth+1) &&
                    (b.left-a.right>=8 || b.top-a.bottom>=8);
            })()'''), "TOTP controls must fit and remain separated"


def draft_input(selector, value, key):
    # Dispatch and inspect in one task: persistence must happen on input, not on blur/unload.
    assert evaluate('''(()=>{
        const field=document.querySelector(%s), value=%s, key=%s;
        field.value=value;field.dispatchEvent(new Event("input",{bubbles:true}));
        const stored=localStorage.getItem(key);
        return value==="" ? stored===null :
            (key===%s ? stored===value : JSON.parse(stored)?.content===value);
    })()''' % tuple(map(json.dumps, (selector, value, key, NEW_KEY)))), "input must persist its raw draft immediately"


def drafts_match(new_value, edit_value, version):
    return evaluate(r'''(()=>{
        const fresh=localStorage.getItem(%s), edit=JSON.parse(localStorage.getItem(%s));
        return fresh===%s && document.getElementById("newContent").value===fresh &&
            edit?.id===1 && /^\d{4}-\d{2}-\d{2}$/.test(edit.create_date) && edit.version===%s &&
            edit.content===%s && document.getElementById("editContent").value===edit.content &&
            !document.getElementById("editor").hidden;
    })()''' % tuple(map(json.dumps, (NEW_KEY, EDIT_KEY, new_value, version, edit_value))))


def write_failure(method, path, button, error_id, unknown=False):
    evaluate('''(()=>{
        window.__writeFetch=window.fetch;window.__writeCalls=0;
        window.fetch=(url,options={})=>{
            if(new URL(String(url),location.href).pathname===%s && options.method===%s){
                window.__writeCalls++;
                return %s;
            }
            return window.__writeFetch(url,options);
        };return true;
    })()''' % (json.dumps(path), json.dumps(method),
               'Promise.reject(new TypeError("Synthetic connection interruption"))' if unknown else
               'Promise.resolve(new Response(JSON.stringify({success:false,data:null,error:"Synthetic write failure"}),{status:500,headers:{"Content-Type":"application/json"}}))'))
    try:
        activate(button)
        wait('document.getElementById(%s).textContent!=="" && !document.querySelector(%s).disabled' %
             (json.dumps(error_id), json.dumps(button)))
        if unknown:
            assert evaluate('document.getElementById(%s).textContent.includes("未知")' % json.dumps(error_id))
        assert evaluate('(async()=>{await new Promise(resolve=>setTimeout(resolve,350));return window.__writeCalls===1})()'), "failed writes must not automatically retry"
    finally:
        evaluate('window.fetch=window.__writeFetch;delete window.__writeFetch;true')


def drafts_cleared():
    return evaluate('localStorage.getItem(%s)===null && localStorage.getItem(%s)===null' %
                    (json.dumps(NEW_KEY), json.dumps(EDIT_KEY)))


def storage_failures(new_value, edit_value, version):
    for error in ("SecurityError", "QuotaExceededError"):
        evaluate('''(()=>{
            window.__setItem=Storage.prototype.setItem;
            Storage.prototype.setItem=function(key,value){
                if(this===localStorage && [%s,%s].includes(key))throw new DOMException("Synthetic storage failure",%s);
                return window.__setItem.call(this,key,value);
            };return true;
        })()''' % tuple(map(json.dumps, (NEW_KEY, EDIT_KEY, error))))
        try:
            for selector, value in (("#newContent", new_value), ("#editContent", edit_value)):
                # CLI fill first emits an empty input (an intentional draft deletion).
                # Inject one nonempty edit so this case tests a failed write, not deletion.
                evaluate('(()=>{const field=document.querySelector(%s);field.value=%s;field.dispatchEvent(new Event("input",{bubbles:true}));return true})()' %
                         (json.dumps(selector), json.dumps(value + " pending")))
                assert evaluate('''(()=>{
                    const status=document.getElementById("draftStatus");
                    return status.getClientRects().length>0 && /失败|无法|不可用|未能/.test(status.textContent) &&
                        document.querySelector(%s).value===%s;
                })()''' % (json.dumps(selector), json.dumps(value + " pending"))), "storage errors must warn and retain the in-memory draft"
        finally:
            evaluate('Storage.prototype.setItem=window.__setItem;delete window.__setItem;true')
        # The failed writes must not replace either previously saved draft.
        assert evaluate('localStorage.getItem(%s)===%s && JSON.parse(localStorage.getItem(%s)).content===%s' %
                        tuple(map(json.dumps, (NEW_KEY, new_value, EDIT_KEY, edit_value))))
        draft_input("#newContent", new_value, NEW_KEY)
        draft_input("#editContent", edit_value, EDIT_KEY)
        assert drafts_match(new_value, edit_value, version)


def gated_restore():
    # Load the real diary assets in a same-origin shell, holding only session reads.
    # This observes the pre-auth DOM without racing the normal login redirect.
    evaluate('''(async()=>{
        const html=await (await fetch("/diary")).text();
        const frame=document.createElement("iframe");frame.id="draftGate";
        const gate='<script>window.__sessionBlocked=false;const nativeFetch=window.fetch;'+
            'const sessionGate=new Promise(resolve=>window.__releaseSession=resolve);'+
            'window.fetch=(url,options)=>new URL(String(url),document.baseURI).pathname==="/session"?'+
            '(window.__sessionBlocked=true,sessionGate.then(()=>nativeFetch(url,options))):nativeFetch(url,options);<\\/script>';
        frame.srcdoc=html.replace("<head>","<head>"+gate);document.body.append(frame);return true;
    })()''')
    try:
        wait('document.getElementById("draftGate").contentWindow.__sessionBlocked===true && document.getElementById("draftGate").contentDocument.readyState==="complete"')
        assert evaluate('''(()=>{
            const doc=document.getElementById("draftGate").contentDocument;
            return doc.getElementById("workspace").hidden && doc.getElementById("newContent").value==="" &&
                doc.getElementById("editContent").value==="";
        })()'''), "drafts must not enter the DOM before session verification"
        evaluate('document.getElementById("draftGate").contentWindow.__releaseSession();true')
        wait('!document.getElementById("draftGate").contentDocument.getElementById("workspace").hidden')
        assert evaluate('''(()=>{
            const doc=document.getElementById("draftGate").contentDocument;
            return doc.getElementById("newContent").value===localStorage.getItem(%s) &&
                doc.getElementById("editContent").value===JSON.parse(localStorage.getItem(%s)).content;
        })()''' % (json.dumps(NEW_KEY), json.dumps(EDIT_KEY)))
    finally:
        evaluate('document.getElementById("draftGate").remove();true')


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
            assert evaluate('(()=>{const diary=document.querySelector("[data-nav-page=diary]"),items=[...document.querySelector(".site-nav").children].filter((item)=>item.matches(".site-link,.site-account,.theme-toggle"));return diary.hidden && items[0].textContent.trim()==="收藏" && items[1]===diary && items[2].textContent.trim()==="登录" && items[3].id==="themeToggle"})()')
            login_layout()
            browser("fill", "#password", password)
            browser("click", "#loginButton")
            wait('!document.getElementById("totpForm").hidden')
            assert evaluate('document.getElementById("loginForm").hidden && document.getElementById("password").value===""')
            assert evaluate('(async()=> (await fetch("/session")).status===401 && (await fetch("/private/diaries")).status===401)()')
            assert evaluate('(()=>{const c=document.getElementById("code");return c.type==="text" && c.inputMode==="numeric" && c.autocomplete==="one-time-code" && c.maxLength===6})()')
            login_layout(second_step=True)
            browser("set", "viewport", "390", "844")
            assert evaluate('document.documentElement.scrollWidth<=window.innerWidth')
            browser("fill", "#code", "000001")
            browser("click", "#backToPassword")
            assert evaluate('!document.getElementById("loginForm").hidden && document.getElementById("totpForm").hidden && document.getElementById("code").value==="" && document.getElementById("password").value===""')
            browser("fill", "#password", password)
            browser("click", "#loginButton")
            wait('!document.getElementById("totpForm").hidden')
            login_step = finish_login(totp_secret)
            wait('location.pathname==="/diary" && !document.getElementById("workspace").hidden && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            wait('document.getElementById("shellAccount").textContent==="管理"')
            assert evaluate('(()=>{const diary=document.querySelector("[data-nav-page=diary]"),items=[...document.querySelector(".site-nav").children].filter((item)=>item.matches(".site-link,.site-account,.theme-toggle"));return !diary.hidden && document.getElementById("shellAccount").getAttribute("href")==="/admin" && items[0].textContent.trim()==="收藏" && items[1]===diary && items[2].textContent.trim()==="管理" && items[3].id==="themeToggle"})()')
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

            assert evaluate('(()=>{const start=document.getElementById("startDate");start.value="2024-01-31";start.dispatchEvent(new Event("change",{bubbles:true}));return document.getElementById("endDate").value==="2024-03-02" && document.getElementById("sort").value==="asc"})()')
            evaluate('window.__filterFetch=window.fetch;window.__filterUrl="";window.fetch=(url,options)=>{if(String(url).startsWith("/private/diaries?"))window.__filterUrl=String(url);return window.__filterFetch(url,options)};true')
            browser("click", '#filterForm button[type="submit"]')
            wait('window.__filterUrl.includes("start_date=2024-01-31") && window.__filterUrl.includes("end_date=2024-03-02") && window.__filterUrl.includes("sort=asc")')
            evaluate('window.fetch=window.__filterFetch;delete window.__filterFetch;true')
            browser("click", "#resetFilters")
            wait('document.getElementById("entries").getAttribute("aria-busy")==="false"')
            print("PASS automatic 31-day diary range and oldest-first filter")

            new_draft = "  synthetic new draft\n😀  "
            edit_draft = "  synthetic edited draft\n😀  "
            draft_input("#newContent", new_draft, NEW_KEY)
            assert evaluate('document.getElementById("draftStatus").textContent.includes("本地") && document.getElementById("draftStatus").textContent.includes("保存")')
            draft_input("#newContent", "", NEW_KEY)
            draft_input("#newContent", 'synthetic <img src=x onerror="window.__xss=1"> 😀', NEW_KEY)
            browser("click", '#createForm button[type="submit"]')
            wait('document.querySelectorAll("#entries article").length===1')
            assert evaluate('window.__xss===undefined && document.querySelectorAll("#entries img").length===0')
            assert drafts_cleared(), "successful creation must clear its draft"
            open_editor()
            draft_input("#editContent", edit_draft, EDIT_KEY)
            draft_input("#newContent", new_draft, NEW_KEY)
            assert drafts_match(new_draft, edit_draft, 1)
            for unknown in (False, True):
                write_failure("POST", "/private/diaries", '#createForm button[type="submit"]', "createError", unknown)
                assert drafts_match(new_draft, edit_draft, 1), "failed creation must preserve both drafts"
                write_failure("PATCH", "/private/diaries/1", "#saveEdit", "editError", unknown)
                assert drafts_match(new_draft, edit_draft, 1), "failed edit must preserve both drafts"
            storage_failures(new_draft, edit_draft, 1)
            browser("reload")
            wait(READY)
            assert drafts_match(new_draft, edit_draft, 1), "reload must restore both raw drafts"
            browser("click", ".site-brand")
            wait('location.pathname==="/"')
            browser("back")
            wait('location.pathname==="/diary"')
            wait(READY)
            assert drafts_match(new_draft, edit_draft, 1), "return navigation must restore both drafts"
            gated_restore()
            print("PASS immediate raw drafts, refresh/navigation, auth gate and storage/write failures without retry")

            # Submit another new entry while retaining the first entry's edit draft.
            activate('#createForm button[type="submit"]')
            wait('document.querySelectorAll("#entries article").length===2 && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            assert evaluate('localStorage.getItem(%s)===null && JSON.parse(localStorage.getItem(%s)).content===%s' %
                            tuple(map(json.dumps, (NEW_KEY, EDIT_KEY, edit_draft)))), "creation must not clear the edit draft"
            evaluate('window.confirm=()=>false;true')
            open_editor('#entries article:first-child button')
            assert evaluate('JSON.parse(localStorage.getItem(%s)).id===1 && document.getElementById("editContent").value===%s' %
                            (json.dumps(EDIT_KEY), json.dumps(edit_draft))), "declining an editor switch must preserve the current draft"
            evaluate('window.confirm=()=>true;true')
            open_editor('#entries article:first-child button')
            assert evaluate('JSON.parse(localStorage.getItem(%s)).id===2' % json.dumps(EDIT_KEY)), "only the confirmed current edit belongs in storage"
            open_editor('#entries article:last-child button')
            draft_input("#editContent", edit_draft, EDIT_KEY)
            # Keep the original soft-delete regression's single-entry fixture.
            assert evaluate('(async()=>{const s=await (await fetch("/session")).json();return (await fetch("/private/diaries/2",{method:"DELETE",headers:{"X-CSRF-Token":s.data.csrf_token}})).status===204})()')
            browser("click", "#resetFilters")
            wait('document.querySelectorAll("#entries article").length===1 && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            draft_input("#newContent", new_draft, NEW_KEY)
            # A separate client changes the resource while the editor keeps version 1.
            assert evaluate('(async()=>{const s=await (await fetch("/session")).json();return (await fetch("/private/diaries/1",{method:"PATCH",headers:{"Content-Type":"application/json","X-CSRF-Token":s.data.csrf_token,"If-Match":JSON.stringify("1")},body:JSON.stringify({content:"synthetic concurrent edit"})})).status===200})()')
            browser("reload")
            wait(READY)
            assert drafts_match(new_draft, edit_draft, 1), "restoring an edit must retain its old version"
            activate("#saveEdit")
            wait('!document.getElementById("conflict").hidden')
            assert drafts_match(new_draft, edit_draft, 1)
            assert evaluate('document.getElementById("saveEdit").disabled')
            activate("#refreshVersion")
            wait('!document.getElementById("latestPanel").hidden')
            assert drafts_match(new_draft, edit_draft, 1), "reading the latest server version must not overwrite a draft"
            activate("#acceptVersion")
            assert drafts_match(new_draft, edit_draft, 2), "accepting a version must persist it without replacing draft text"
            activate("#saveEdit")
            wait('document.getElementById("editor").hidden && document.getElementById("entries").textContent.includes("synthetic edited draft")')
            assert evaluate('localStorage.getItem(%s)===null && localStorage.getItem(%s)===%s' %
                            tuple(map(json.dumps, (EDIT_KEY, NEW_KEY, new_draft)))), "successful edit must not clear the new draft"
            print("PASS create, plain-text XSS defence, conflict-preserving edit")

            open_editor()
            draft_input("#editContent", edit_draft, EDIT_KEY)
            evaluate('window.confirm=()=>true;true')
            activate("#cancelEdit")
            assert evaluate('document.getElementById("editor").hidden && localStorage.getItem(%s)===null && localStorage.getItem(%s)===%s' %
                            tuple(map(json.dumps, (EDIT_KEY, NEW_KEY, new_draft)))), "explicit cancellation must only clear the edit draft"
            open_editor()
            draft_input("#editContent", edit_draft, EDIT_KEY)
            # Revoke this synthetic session outside the UI, then exercise a genuine 401.
            assert evaluate('(async()=>{const s=await (await fetch("/session")).json();return (await fetch("/session",{method:"DELETE",headers:{"X-CSRF-Token":s.data.csrf_token}})).status===204})()')
            browser("click", "#resetFilters")
            wait('location.pathname==="/login" && location.search==="?next=%2Fdiary" && !document.getElementById("loginForm").hidden')
            assert evaluate('localStorage.getItem(%s)===%s && JSON.parse(localStorage.getItem(%s)).content===%s && !document.querySelector("#newContent,#editContent")' %
                            tuple(map(json.dumps, (NEW_KEY, new_draft, EDIT_KEY, edit_draft)))), "401 must preserve drafts without displaying them on login"
            login_step = login(password, totp_secret, login_step)
            wait(READY)
            assert drafts_match(new_draft, edit_draft, 3), "reauthentication must restore both drafts"
            print("PASS targeted draft cleanup, real session expiry and replay-safe reauthentication")

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
            assert evaluate('localStorage.getItem(%s)===null && localStorage.getItem(%s)===%s' %
                            tuple(map(json.dumps, (EDIT_KEY, NEW_KEY, new_draft)))), "deleting the edited entry must clear only its edit draft"
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

            for malformed in ("{broken", '{"id":1,"create_date":"invalid","version":0,"content":false}'):
                evaluate('localStorage.setItem(%s,%s);true' % (json.dumps(EDIT_KEY), json.dumps(malformed)))
                browser("reload")
                wait(READY)
                assert evaluate('''(()=>{
                    const status=document.getElementById("draftStatus");
                    return document.getElementById("editor").hidden && status.getClientRects().length>0 &&
                        /损坏|失败|无法/.test(status.textContent) && document.getElementById("newContent").value===%s &&
                        !document.querySelector('#createForm button[type="submit"]').disabled;
                })()''' % json.dumps(new_draft)), "malformed stored edits must warn without breaking the new draft or UI"
            evaluate('localStorage.removeItem(%s);true' % json.dumps(EDIT_KEY))
            activate('#createForm button[type="submit"]')
            wait('document.querySelectorAll("#entries article").length===1 && document.getElementById("entries").getAttribute("aria-busy")==="false"')
            open_editor()
            draft_input("#editContent", edit_draft, EDIT_KEY)
            draft_input("#newContent", new_draft, NEW_KEY)
            print("PASS malformed JSON and invalid edit metadata recovery")

            # A real POST commits, but the response is lost: preserve the draft and do not retry.
            evaluate('''window.__lostFetch=window.fetch;window.__lostCalls=0;
                window.fetch=async(url,options={})=>{
                    if(String(url)==="/private/diaries" && options.method==="POST"){
                        window.__lostCalls++;
                        const response=await window.__lostFetch(url,options);
                        if(response.status!==201)throw new Error("Synthetic fixture creation failed");
                        throw new TypeError("Synthetic response lost after commit");
                    }
                    return window.__lostFetch(url,options);
                };true''')
            try:
                activate('#createForm button[type="submit"]')
                wait('document.getElementById("createError").textContent.includes("未知") && !document.querySelector(\'#createForm button[type="submit"]\').disabled')
                assert evaluate('(async()=>{await new Promise(resolve=>setTimeout(resolve,350));return window.__lostCalls===1 && (await (await fetch("/private/diaries")).json()).data.total===2})()'), "lost response must not trigger a duplicate POST"
                assert evaluate('localStorage.getItem(%s)===%s && JSON.parse(localStorage.getItem(%s)).content===%s' %
                                tuple(map(json.dumps, (NEW_KEY, new_draft, EDIT_KEY, edit_draft))))
            finally:
                evaluate('window.fetch=window.__lostFetch;delete window.__lostFetch;true')
            browser("reload")
            wait(READY)
            assert evaluate('document.querySelectorAll("#entries article").length===2 && document.getElementById("newContent").value===%s && document.getElementById("editContent").value===%s' %
                            (json.dumps(new_draft), json.dumps(edit_draft)))
            print("PASS committed-but-lost response preserves both drafts without duplicate creation")

            evaluate('window.confirm=()=>true;true')
            write_failure("DELETE", "/session", "#logout", "notice")
            assert evaluate('localStorage.getItem(%s)===%s && JSON.parse(localStorage.getItem(%s)).content===%s' %
                            tuple(map(json.dumps, (NEW_KEY, new_draft, EDIT_KEY, edit_draft)))), "unconfirmed diary logout must retain both drafts"
            evaluate('window.__realFetch=window.fetch;window.fetch=(url,opts={})=>String(url).startsWith("/private/diaries?")&&(!opts.method||opts.method==="GET")?new Promise((resolve,reject)=>opts.signal.addEventListener("abort",()=>reject(new DOMException("Aborted","AbortError")))):window.__realFetch(url,opts);true')
            browser("click", "#resetFilters")
            wait('document.getElementById("entries").getAttribute("aria-busy")==="true"')
            assert evaluate('!document.getElementById("logout").disabled')
            browser("click", "#logout")
            wait('location.pathname==="/login" && location.search==="?next=%2Fdiary" && !document.getElementById("loginForm").hidden')
            assert evaluate('(async()=>({ok:(await fetch("/session")).status===401}))()')["ok"]
            assert drafts_cleared(), "confirmed diary logout must clear both drafts"
            print("PASS pending-list cancellation and persisted logout")

            login_step = login(password, totp_secret, login_step)
            wait(READY)
            assert evaluate('document.getElementById("newContent").value==="" && document.getElementById("editor").hidden'), "cleared drafts must not reappear after login"
            draft_input("#newContent", new_draft, NEW_KEY)
            open_editor('#entries article:first-child button')
            draft_input("#editContent", edit_draft, EDIT_KEY)
            browser("click", "#shellAccount")
            wait('location.pathname==="/admin" && !document.getElementById("settingsForm").hidden')
            evaluate('window.confirm=()=>true;true')
            write_failure("DELETE", "/session", "#logout", "settingsError")
            assert evaluate('localStorage.getItem(%s)===%s && JSON.parse(localStorage.getItem(%s)).content===%s' %
                            tuple(map(json.dumps, (NEW_KEY, new_draft, EDIT_KEY, edit_draft)))), "unconfirmed admin logout must retain both drafts"
            activate("#logout")
            wait('location.pathname==="/login" && location.search==="?next=%2Fadmin" && !document.getElementById("loginForm").hidden')
            assert drafts_cleared(), "confirmed admin logout must clear both drafts"
            assert evaluate('(async()=>(await fetch("/session")).status===401)()')
            print("PASS admin logout clears both drafts only after server confirmation")
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
    try:
        main()
    except Exception as error:
        # Source excerpts/exception payloads can contain synthetic credentials or bodies.
        # Report locations only; never dump browser output or local variables.
        trace = error.__traceback__
        locations = []
        while trace is not None:
            locations.append(f"{trace.tb_frame.f_code.co_name}:{trace.tb_lineno}")
            trace = trace.tb_next
        raise SystemExit(f"FAIL {type(error).__name__} at {' -> '.join(locations)}") from None
