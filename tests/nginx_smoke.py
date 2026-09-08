"""Optional isolated production-proxy check: python3 -B tests/nginx_smoke.py [nginx].

Uses synthetic credentials/certificates and temporary prefixes only. Does not
read or reload the system Nginx configuration or touch any existing service.
"""
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/release/memento'
NGINX = sys.argv[1] if len(sys.argv) > 1 else 'nginx'


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def code_for(secret):
    key = base64.b32decode(secret)
    digest = hmac.new(key, (int(time.time()) // 30).to_bytes(8, 'big'), hashlib.sha1).digest()
    offset = digest[-1] & 15
    return str((int.from_bytes(digest[offset:offset + 4], 'big') & 0x7fffffff) % 1000000).zfill(6)


def run_case(tls):
    with tempfile.TemporaryDirectory(prefix='memento-nginx-') as name:
        directory = Path(name)
        upstream, port, redirect = free_port(), free_port(), free_port()
        origin = ('https' if tls else 'http') + '://127.0.0.1:' + str(port)
        password, key = secrets.token_urlsafe(24), secrets.token_hex(32)
        secret = base64.b32encode(secrets.token_bytes(20)).decode()
        hashed = subprocess.run([str(BINARY), 'hash-password', '--stdin'], input=password,
                                text=True, capture_output=True, check=True).stdout.strip()
        # Exercise actual selected-file loading, not just injected process variables.
        values = dict(MEMENTO_BIND=f'127.0.0.1:{upstream}', MEMENTO_DB_PATH=str(directory / 'test.db'),
                      MEMENTO_LOGIN_USERNAME='qa', MEMENTO_PASSWORD_HASH=hashed,
                      MEMENTO_TOTP_SECRET=secret, MEMENTO_PUBLIC_ORIGIN=origin,
                      MEMENTO_API_KEY=key, MEMENTO_SESSION_TTL_DAYS='2', RUST_LOG='warn')
        with os.fdopen(os.open(directory / '.env.production', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as file:
            file.write(''.join(f"{k}='{v}'\n" for k, v in values.items()))
        template = 'memento-https.conf' if tls else 'memento.conf'
        snippet = (ROOT / 'deploy/nginx' / template).read_text()
        snippet = snippet.replace('127.0.0.1:23457', f'127.0.0.1:{upstream}')
        snippet = snippet.replace('/var/log/nginx/memento-error.log', str(directory / 'error.log'))
        context = None
        if tls:
            cert, private_key = directory / 'test.crt', directory / 'test.key'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                            '-keyout', str(private_key), '-out', str(cert), '-days', '1',
                            '-subj', '/CN=localhost', '-addext', 'subjectAltName=IP:127.0.0.1,DNS:localhost'],
                           check=True, capture_output=True)
            snippet = snippet.replace('listen 443 ssl;', f'listen 127.0.0.1:{port} ssl;')
            snippet = snippet.replace('listen 80;', f'listen 127.0.0.1:{redirect};')
            snippet = snippet.replace('/path/to/fullchain.pem', str(cert)).replace('/path/to/privkey.pem', str(private_key))
            context = ssl.create_default_context(cafile=str(cert))
        else:
            snippet = snippet.replace('listen 8080;', f'listen 127.0.0.1:{port};')
        (directory / 'site.conf').write_text(snippet)
        main = directory / 'nginx.conf'
        main.write_text(f'worker_processes 1;\npid {directory}/nginx.pid;\nerror_log {directory}/error.log warn;\n'
                        f'events {{ worker_connections 64; }}\nhttp {{ access_log off; client_body_temp_path {directory}/body; '
                        f'proxy_temp_path {directory}/proxy; fastcgi_temp_path {directory}/fastcgi; '
                        f'uwsgi_temp_path {directory}/uwsgi; scgi_temp_path {directory}/scgi; '
                        f'include {directory}/site.conf; }}\n')
        command = [NGINX, '-e', 'stderr', '-p', str(directory) + '/', '-c', str(main)]
        checked = subprocess.run(command + ['-t'], capture_output=True, text=True)
        if checked.returncode:
            raise AssertionError('isolated nginx -t failed: ' + checked.stderr)
        app = subprocess.Popen([str(BINARY)], cwd=directory, env={'MEMENTO_ENV': 'production'},
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        proxy = subprocess.Popen(command + ['-g', 'daemon off;'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))

        def request(method, path, body=None, headers=None, raw=None):
            fields = dict(headers or {})
            if body is not None:
                raw = json.dumps(body).encode()
                fields['Content-Type'] = 'application/json'
            req = urllib.request.Request(origin + path, data=raw, headers=fields, method=method)
            try:
                response = opener.open(req, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                return response.status, response.headers, response.read()

        try:
            for _ in range(100):
                if app.poll() is not None or proxy.poll() is not None:
                    raise AssertionError('temporary app or Nginx exited')
                try:
                    if request('GET', '/api/health')[0] == 200:
                        break
                except OSError:
                    pass
                time.sleep(0.05)
            else:
                raise AssertionError('proxy readiness timed out')
            assert request('GET', '/diary')[0] == 200
            assert request('POST', '/session', {'username': 'qa', 'password': password}, {'Origin': 'http://other.test'})[0] == 403
            status, headers, data = request('POST', '/session', {'username': 'qa', 'password': password}, {'Origin': origin})
            assert status == 200 and headers.get('Set-Cookie') is None
            challenge = json.loads(data)['data']['challenge']
            status, headers, _ = request('POST', '/session', {'challenge': challenge, 'code': code_for(secret)}, {'Origin': origin})
            assert status == 200 and headers['Cache-Control'] == 'no-store'
            cookie = headers['Set-Cookie']
            assert 'SameSite=Strict' in cookie and 'HttpOnly' in cookie and 'Max-Age=172800' in cookie
            assert ('; Secure' in cookie) == tls
            cookie = cookie.split(';')[0]
            status, _, data = request('GET', '/session', headers={'Cookie': cookie})
            assert status == 200
            csrf = json.loads(data)['data']['csrf_token']
            browser_headers = {'Cookie': cookie, 'Origin': origin, 'X-CSRF-Token': csrf}
            assert request('POST', '/private/diaries', {'content': 'synthetic proxy entry'}, browser_headers)[0] == 201
            status, _, data = request('GET', '/api/diaries', headers={'X-API-Key': key})
            assert status == 200
            item = json.loads(data)['data']['items'][0]
            assert request('PATCH', '/api/diaries/' + str(item['id']), {'content': 'synthetic changed'}, {'X-API-Key': key})[0] == 200
            assert request('DELETE', '/api/diaries/' + str(item['id']), headers={'X-API-Key': key})[0] == 204
            assert request('GET', '/api/diaries/' + str(item['id']), headers={'X-API-Key': key})[0] == 404
            assert request('DELETE', '/session', headers=browser_headers)[0] == 204
            assert request('GET', '/private/diaries', headers={'Cookie': cookie})[0] == 401
            assert request('POST', '/session', headers={'Origin': origin}, raw=b'x' * 8193)[0] == 413
            assert request('POST', '/api/diaries', headers={'X-API-Key': key}, raw=b'x' * 65537)[0] == 413
            statuses = [request('POST', '/session', {'username': 'qa', 'password': 'wrong'}, {'Origin': origin})[0] for _ in range(14)]
            assert 429 in statuses
            # Exhausting password/OTP attempts must not block status checks or logout.
            assert request('GET', '/session')[0] == 401
            assert request('DELETE', '/session', headers=browser_headers)[0] == 401
            print('PASS Nginx ' + ('HTTPS' if tls else 'HTTP') + ': syntax, env loading, two-step login, cookies, diary API, body limits, rate limit')
        finally:
            proxy.terminate()
            app.terminate()
            proxy.wait(timeout=10)
            app.wait(timeout=10)


if __name__ == '__main__':
    run_case(False)
    run_case(True)
