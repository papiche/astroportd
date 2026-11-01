"""Test de fumée navigateur de la table de tri et de la réplication : API simulée, fichiers statiques RÉELS de la coque.

Chromium (Playwright) pilote l'application : sélection par groupe, rangement, glisser-déposer vers une pochette, lasso,
« pas à reconnaître », écran de réplication, thèmes clair/sombre, mobile. Aucune erreur de console n'est tolérée.

    python3 tests/browser/smoke_ui.py [dossier-des-captures]

Hors de la CI (Playwright et Chromium requis) : à lancer avant de publier une modification de `crates/astro-ui/web/`.
"""
import base64, io, json, math, random, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from PIL import Image, ImageDraw
from playwright.sync_api import sync_playwright

WEB = Path(__file__).resolve().parents[2] / 'crates' / 'astro-ui' / 'web'
OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
random.seed(7)
MIME = {'.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.webmanifest': 'application/manifest+json'}

def make_faces():
    faces, fid = [], 1
    def add(name, pub, group, gsize, x, y, named, sug=None):
        nonlocal fid
        faces.append(dict(id=fid, name=name, pubkey=pub, named=named, path=f'/Photos/{fid}.jpg', bbox=[10, 10, 100, 100], x=x, y=y, group_id=group, group_size=gsize, suggest=sug))
        fid += 1
    for pi, name in enumerate(['Marie', 'Paul', 'Léa']):
        for _ in range(14 + pi * 6): add(name, f'{pi:064x}', None, None, random.uniform(-1, 1), random.uniform(-1, 1), True)
    for gi in range(10):
        n = random.randint(2, 9)
        for _ in range(n): add(f'Inconnu_{fid:08d}', None, f'g{gi:011d}', n, random.uniform(-1, 1), random.uniform(-1, 1), False)
    for _ in range(80): add(f'Inconnu_{fid:08d}', None, None, None, random.uniform(-1, 1), random.uniform(-1, 1), False)
    faces[-1]['suggest'] = dict(name='Marie', score=0.71)
    return faces

COOP = dict(exists=True, updated_at=1790000000, did='did:nostr:' + 'ab' * 32, unmanaged=['ZZ_LEGACY'], sections=[dict(title='Règle 3×1/3', fields=[dict(key=k, value=v, secret=False) for k, v in [('TREASURY_PERCENT', '33.33'), ('RND_PERCENT', '33.33'), ('ASSETS_PERCENT', '33.33'), ('CAPTAIN_BONUS_PERCENT', '1')]]), dict(title='OpenCollective', fields=[dict(key='OCSLUG', value='uplanet', secret=False), dict(key='OCAPIKEY', secret=True, set=True, readable=True)])])
ADMIN = dict(face=dict(mode='off', label=None, locked=None, onnx_compiled=True, models='/m', library=None, pending=3, failed=0),
             kinds=dict(kinds=[dict(kind=1, label='Note publique', read='public', write='circle', project=True, default='read=public write=circle project=on', source='défaut'), dict(kind=7, label='Réaction (+N = Ẑen)', read='circle', write='circle', project=True, default='read=circle write=circle project=on', source='défaut')],
                        choices=dict(read=['public', 'addressed', 'friends', 'circle', 'deny'], write=['circle', 'open', 'deny']), fixed={'1059': 'x', '22242': 'y'}))
ADMIN.update(tunnels=None, tasks=None, assist=dict(services=['ssh'], friends=[], incoming=[], outgoing=[]))  # réglages facultatifs de m-options.js : absents, l'écran s'en passe sans erreur
ACCOUNT = dict(hex='aa' * 32, npub='npub1test', email='alice@example.org', role='multipass', g1pub='G1TEST', home_station='Qm:ab', lang='fr', langs=['fr', 'en', 'es'], profile=dict(name='Alice', about='Bonjour', picture=None),
               dav=dict(url='/dav', login='alice@example.org'), token_generation=0, keys_exportable=True, has_g1_key=True, relays=[dict(url='wss://s.example', read=True, write=True)])
CONTACTS = dict(contacts=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=['lea@example.org'], pubkey='ab' * 32, via='vcard', g1pub=None, warning=None, hosted_here=None, friends=True, faces=12, faces_linked=12),
                          dict(path='/Contacts/Paul.vcf', name='Paul', emails=[], pubkey=None, via=None, g1pub=None, warning=None, hosted_here=None, friends=False, faces=3, faces_linked=0)],
                people_without_contact=[dict(name='Nina', faces=4)], carddav=dict(url='https://s.example/dav/addressbooks/alice%40example.org/contacts/', server='https://s.example/dav/', login='alice@example.org'))
INVITES = dict(invites=[dict(token='1' * 32, label='Marc', email=None, contact_path=None, count=5, state='claimed', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=1790000200, url='https://s.example/i/' + '1' * 32),
                        dict(token='2' * 32, label='Zoé', email=None, contact_path=None, count=3, state='opened', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=None, url='https://s.example/i/' + '2' * 32)])
TOKEN = 'a1' * 16
QR = '<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>'
CARD = 'astro-share:v1|alice@example.org|' + 'ab' * 32 + '|isolated|0123456789abcdef|1|c2VjcmV0c2VjcmV0|abcd'
STATE = dict(faces=make_faces(), posts=[], deleted=[])

def people():
    by = {}
    for f in STATE['faces']:
        if f['named']:
            p = by.setdefault(f['pubkey'] or f['name'], dict(name=f['name'], pubkey=f['pubkey'], faces=0, photos=set()))
            p['faces'] += 1; p['photos'].add(f['path'])
    return sorted([dict(name=p['name'], pubkey=p['pubkey'], faces=p['faces'], photos=len(p['photos'])) for p in by.values()], key=lambda p: -p['photos'])

def thumb(seed):
    im = Image.new('RGB', (96, 96), tuple(random.Random(seed).randrange(60, 220) for _ in range(3)))
    d = ImageDraw.Draw(im); d.ellipse((24, 20, 72, 80), fill=(240, 210, 190)); d.ellipse((38, 40, 46, 48), fill=(40, 40, 40)); d.ellipse((54, 40, 62, 48), fill=(40, 40, 40))
    b = io.BytesIO(); im.save(b, 'JPEG'); return 'data:image/jpeg;base64,' + base64.b64encode(b.getvalue()).decode()

REPL = dict(mine=dict(at=1, events=30, replicated=27, unreachable=1, peers=[
    dict(node_id='12D3A', name='poppy', captain='ami@example.org', level=1, have=27, error=None),
    dict(node_id='12D3B', name='sagittarius', captain='fred@example.org', level=2, have=12, error=None),
    dict(node_id='12D3C', name='perdue', captain=None, level=None, have=None, error='tunnel : connexion fermée')],
    media=dict(files=8, replicated=5, lookup_failed=1, peers=[dict(node_id='12D3A', name='poppy', have=5)])),
    crawl=dict(at=1, ms=4200, relays=3, keys=40, fetched=120, inserted=12, errors=[dict(relay='p2p://12D3C', key='ab', reason='connexion fermée')]),
    routes=[dict(target='p2p://12D3A|wss://relay.poppy.example', via='p2p', at=1, error=None), dict(target='p2p://12D3C', via='down', at=1, error='tunnel : connexion fermée')])

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body, ctype='application/json'):
        if not isinstance(body, bytes): body = json.dumps(body).encode()
        self.send_response(code); self.send_header('Content-Type', ctype); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        p = self.path.split('?')[0]
        if p == '/': return self.send(200, (WEB / 'index.html').read_bytes(), 'text/html')
        if p == '/register': return self.send(200, (WEB / 'register.html').read_bytes(), 'text/html')
        if p == '/api/v1/register': return self.send(200, dict(open=True, constellation='ORIGIN', accounts=1, max_accounts=500))
        if p == '/restore': return self.send(200, (WEB / 'restore.html').read_bytes(), 'text/html')
        if p == '/claim': return self.send(200, (WEB / 'claim.html').read_bytes(), 'text/html')
        if p.startswith('/i/'): return self.send(200, (WEB / 'invite.html').read_bytes(), 'text/html')
        if p == '/api/v1/invites': return self.send(200, INVITES)
        if p == '/api/v1/invite/' + 'b2' * 16: return self.send(410, dict(error='ce lien a expiré : demandez-en un nouveau', code='invite_expired'))
        if p == f'/api/v1/invite/{TOKEN}': return self.send(200, dict(**{'from': 'alice'}, label='Léa', count=4, photos=4, thumbs=[0, 1], expires_at=1791000000, email_hint='l**@example.org'))
        if p.startswith(f'/api/v1/invite/{TOKEN}/thumb/'):
            im = Image.new('RGB', (16, 12), (180, 90, 60)); b = io.BytesIO(); im.save(b, 'PNG'); return self.send(200, b.getvalue(), 'image/png')
        if p.startswith('/ui/'):
            f = WEB / p[4:]
            return self.send(200, f.read_bytes(), MIME.get(f.suffix, 'application/octet-stream')) if f.is_file() else self.send(404, b'x', 'text/plain')
        if p == '/api/v1/me':
            if not (self.headers.get('Authorization') or '').startswith(('Basic ', 'Nostr ')): return self.send(401, dict(error='authentification requise'))
            return self.send(200, dict(hex='aa' * 32, email='alice@example.org', captain=True, ai=STATE.get('ai', True)))
        if p == '/api/v1/me/ai': return self.send(200, dict(ollama_url=None, provider=None, has_key=False, model=None, declared=STATE.get('ai', True)))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/account': return self.send(200, ACCOUNT)
        if p == '/api/v1/contacts': return self.send(200, CONTACTS)
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/faces/folders': return self.send(200, dict(folders=[dict(dir='/Images/2026/10', faces=12, todo=9), dict(dir='/Images/2026/09', faces=4, todo=0)]))
        if p == '/api/v1/faces/board':
            import time; time.sleep(STATE.get('delay', 0))
            return self.send(200, dict(enabled=True, dav='/dav', faces=STATE['faces'], people=people(), unknown=sum(1 for f in STATE['faces'] if not f['named']), groups_skipped=False, truncated=False))
        if p.startswith('/dav/'): return self.send(200, base64.b64decode('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'), 'image/gif')
        if p == '/api/v1/station/planes': return self.send(200, dict(configured=True, multi_process=True, front=True, planes=[dict(name='origin', origin=True, http='127.0.0.1:18080', db='/x/uplanet.db', db_exists=True, running=True), dict(name='zen1', origin=False, http='127.0.0.1:18081', db='/x/zen1/uplanet.db', db_exists=False, running=False)], restart_needed=True, pending=['zen1'], path='/x/planes.json'))
        if p.startswith('/api/v1/book'):
            STATE.setdefault('gets', []).append(self.path)
            lea = dict(name='Léa Martin', given='Léa', family='Martin', org='Atelier', title='', phones=[dict(kind='mobile', value='+33 6 12 34 56 78')], emails=[dict(kind='home', value='lea@example.org')], addresses=[], birthday='', note='', urls=[], groups=['Famille'], about='', picture='', nip05='', lud16='')
            if p == '/api/v1/book': return self.send(200, dict(total=3, letters=['L', 'P'], groups=[dict(name='Famille', count=1)], contacts=[
                dict(path='/Contacts/Moi.vcf', name='Alice', org='', title='', emails=['alice@example.org'], phones=[], groups=[], letter='A', me=True, pubkey='aa' * 32, via=None, warning=None, faces=0, picture=''),
                dict(path='/Contacts/Lea.vcf', name='Léa Martin', org='Atelier', title='', emails=['lea@example.org'], phones=lea['phones'], groups=['Famille'], letter='L', me=False, pubkey='ab' * 32, via='vcard', warning=None, faces=12, picture=''),
                dict(path='/Contacts/Paul.vcf', name='Paul Durand', org='', title='', emails=[], phones=[], groups=[], letter='P', me=False, pubkey=None, via=None, warning=None, faces=0, picture='')]))
            if p == '/api/v1/book/me': return self.send(200, dict(path='/Contacts/Moi.vcf', me=True, pubkey='aa' * 32, fields=dict(lea, name='Alice', given='Alice', family='', org='', phones=[], emails=[dict(kind='home', value='alice@example.org')], groups=[], about='Capitaine'), faces=0, hosted_here='alice@example.org', profile=None, diff=None, drift=False))
            if p == '/api/v1/book/card': return self.send(200, dict(path='/Contacts/Lea.vcf', me=False, pubkey='ab' * 32, via='vcard', fields=lea, faces=12, hosted_here=None, profile=dict(name='Léa M.'), drift=True, diff=[
                dict(key='name', label='Nom', mine='Léa Martin', theirs='Léa M.', same=False, can_take=True), dict(key='about', label='Présentation', mine='', theirs='Boulangère', same=False, can_take=True), dict(key='nip05', label='NIP-05', mine='', theirs='', same=True, can_take=False)]))
            if p == '/api/v1/book/duplicates': return self.send(200, dict(groups=[dict(reasons=['même téléphone'], cards=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=[], phones=['+33 6 12']), dict(path='/Contacts/Lea2.vcf', name='L. Martin', emails=[], phones=['06 12'])])]))
        if p == '/api/v1/ucloud/memories': return self.send(200, dict(memories=[]))
        if p == '/api/v1/ucloud/timeline':
            q = self.path.split('ym=')[1] if 'ym=' in self.path else None
            if q is None: return self.send(200, dict(months=[dict(ym='2026-10', count=3), dict(ym='2026-09', count=2)]))
            base = 1790000000 if q == '2026-10' else 1787000000
            return self.send(200, dict(items=[dict(path=f'/Photos/{q}-{i}.jpg', mime='image/jpeg', size=10, mtime=base, taken_at=base - i * 90000, t=base - i * 90000, thumb=None, lat=None, lon=None) for i in range(3 if q == '2026-10' else 2)]))
        if p == '/api/v1/ucloud/map':
            if 'items=1' in self.path: return self.send(200, dict(items=[dict(path='/Photos/toulouse.jpg', mime='image/jpeg', size=10, mtime=1790000000, taken_at=1790000000, t=1790000000, thumb=None, lat=43.6, lon=1.44)]))
            return self.send(200, dict(points=[dict(lat=43.6, lon=1.44, n=12, t0=1, t1=2), dict(lat=35.68, lon=139.69, n=3, t0=1, t1=2), dict(lat=-33.9, lon=151.2, n=1, t0=1, t1=2)], months=[dict(ym='2026-09', n=6), dict(ym='2026-10', n=10)], geotagged=16, photos=40))
        if p == '/api/v1/ucloud/duplicates': return self.send(200, dict(exact=[], similar=[]))
        if p == '/api/v1/replication': return self.send(200, REPL)
        if p == '/api/v1/devices': return self.send(200, dict(devices=[dict(id='d1', label='Téléphone de Léa', root='/Photos/Léa', mode='write', created_at=1790000000, last_used=None)], max=16))
        if p == '/api/v1/organize': return self.send(200, dict(folders=[], job=None, runs=[dict(run_id='r1', at=1790000000, moved=7, undone=False, options={})]))
        if p == '/api/v1/pull': return self.send(200, dict(sources=[dict(id='s1', label='Mon Nextcloud', url='https://nc.example/remote.php/dav/files/fred', username='fred', remote_path='Photos', dest_prefix='/Imports/Mon Nextcloud', private_ok=False,
            job=dict(state='running', dry_run=False, started=1, finished=None, progress=dict(done=40, total=100, imported=30, skipped=8, errors=2), report=None, error=None), last=None)]))
        if p == '/api/v1/backup': return self.send(200, dict(mine=None, friends=[], hosting=[], share=None))
        if p == '/api/v1/faces/pending-shares': return self.send(200, dict(pending=[]))
        if p == '/api/v1/activity': return self.send(200, b': ok\n\n', 'text/event-stream')
        self.send(404, dict(error='simulé : ' + p))
    def do_DELETE(self):
        STATE['deleted'].append(self.path); self.send(200, dict(revoked=True))
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); body = json.loads(self.rfile.read(n) or b'{}'); p = self.path
        STATE['posts'].append((p, body))
        if p == '/api/v1/faces/thumbs': return self.send(200, dict(thumbs={str(i): thumb(i) for i in body['ids']}, failed=[]))
        if p == '/api/v1/station/planes': return self.send(200, dict(name=body['name'], http='127.0.0.1:18082', p2p='/ip4/0.0.0.0/tcp/4102', swarm_key_path='/x/zen2/swarm.key', created_key='/key/swarm/psk/1.0.0/\\n/base16/\\n' + 'ab' * 32))
        if p == '/api/v1/me/ai':
            STATE['ai'] = bool(body.get('ollama_url') or body.get('provider')); return self.send(200, dict(ollama_url=body.get('ollama_url'), provider=body.get('provider'), has_key=False, model=None, declared=STATE['ai']))
        if p.startswith('/api/v1/book'): return self.send(200, dict(path='/Contacts/Lea.vcf', taken=body.get('keys'), imported=2, duplicates=1, rejected=[], merged=True, removed=1, changed=1, fields=body.get('fields') or body))
        if p == '/api/v1/faces/delete-photos': return self.send(200, dict(deleted=len({f['path'] for f in STATE['faces'] if f['id'] in body['ids']}), missing=0, bytes=0))
        if p == '/api/v1/faces/assign':
            for f in STATE['faces']:
                if f['id'] in body['ids']: f.update(name=body['name'], pubkey=body.get('pubkey'), named=True, group_id=None, group_size=None, suggest=None)
            return self.send(200, dict(assigned=len(body['ids']), name=body['name']))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/devices': return self.send(200, dict(id='d2', label=body['label'], root=body['root'], mode=body['mode'], token='dv_' + 'ab' * 16, note='Ce jeton ne sera plus jamais affiché : copiez-le maintenant.'))
        if p == '/api/v1/devices/revoke': return self.send(200, dict(revoked=body['id']))
        if p == '/api/v1/organize/preview': return self.send(200, dict(total=7, placed=1, protected=4, unknown=1, capped=False, by_kind=[dict(kind='images', folder='Images', files=2, bytes=3000, sample=[dict(**{'from': '/Imports/A/p.jpg', 'to': '/Images/p.jpg'})]), dict(kind='documents', folder='Documents', files=5, bytes=900, sample=[])]))
        if p == '/api/v1/organize/undo': return self.send(200, dict(restored=7))
        if p.startswith('/api/v1/organize') or p.startswith('/api/v1/pull'): return self.send(200, dict(ok=True))
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/register': return self.send(200, dict(email='lea-x7k2p9@uplanet.local', token='JT', pass_='123456', salt='S', pepper='P', nsec='nsec1x', npub='npub1x', hex='cd' * 32, g1pub='G', nostrns='/ipns/k', constellation='ORIGIN', lang='fr', guest=True, card=CARD, card_svg=QR, **{'pass': '123456'}))
        if p == '/api/v1/restore': return self.send(200, dict(email='alice@example.org', token='JT2', hex='ab' * 32, constellation='ORIGIN', restoring=True, **{'pass': '654321'}))
        if p == '/api/v1/invites': return self.send(200, dict(token=TOKEN, count=3, label=body.get('label'), expires_at=1791000000, url='https://s.example/i/' + TOKEN, qr_svg=QR))
        if p == '/api/v1/invites/remote-accept': return self.send(200, dict(**{'from': 'alice'}, origin=body['origin'], sent=4, warnings=[]))
        if p == f'/api/v1/invite/{TOKEN}/accept': return self.send(200, dict(**{'from': 'alice'}, shared=4, warnings=[]))
        if p == f'/api/v1/invite/{TOKEN}/claim':
            if body.get('email') == 'taken@example.org': return self.send(409, dict(error='adresse déjà hébergée', code='email_taken'))
            return self.send(200, dict(email=body['email'], token='JETON-WEB', pass_='1234', salt='SEL', pepper='POIVRE', nsec='nsec1secret', npub='npub1x', hex='cd' * 32, g1pub='G1X', nostrns='/ipns/k51', constellation='ORIGIN', lang=body['lang'], **{'from': 'alice'}, shared=4, warnings=[]))
        if p.startswith('/api/v1/contacts/'): return self.send(200, dict(path='/Contacts/x.vcf', linked=1))
        if p == '/api/v1/account/unlock':
            if body.get('pass') != '123456': return self.send(401, dict(error='code PASS incorrect : 4 essai(s) restant(s) avant verrouillage'))
            return self.send(200, dict(email=body['email'], hex='aa' * 32, token='TOKEN-SESSION', guest=False))
        if p == '/api/v1/account/sign': return self.send(200, dict(id='11' * 32, pubkey='aa' * 32, sig='22' * 64, created_at=1, **{k: body[k] for k in ('kind', 'content', 'tags')}))
        if p == '/api/v1/account/token': return self.send(200, dict(login='alice@example.org', token='NOUVEAU-JETON', generation=1))
        if p == '/api/v1/account/keys': return self.send(200, dict(nsec='nsec1secret', npub='npub1test', dunikey='sec: x', dunikey_error=None))
        if p == '/api/v1/account/profile': return self.send(200, ACCOUNT)
        if p == '/api/v1/faces/forget':
            STATE['faces'] = [f for f in STATE['faces'] if f['id'] not in body['ids']]; return self.send(200, dict(forgotten=len(body['ids'])))
        self.send(404, dict(error='simulé : ' + p))

srv = ThreadingHTTPServer(('127.0.0.1', 0), H); port = srv.server_address[1]
threading.Thread(target=srv.serve_forever, daemon=True).start()
errors, failures = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

with sync_playwright() as pw:
    br = pw.chromium.launch(); ctx = br.new_context(viewport=dict(width=1280, height=820), service_workers='block'); page = ctx.new_page()
    page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
    ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(900)
    check(page.inner_text('.pe-count').startswith(f"{sum(1 for f in STATE['faces'] if not f['named'])} à ranger"), 'le compteur « à ranger » est affiché : ' + page.inner_text('.pe-count'))
    check(page.locator('.pe-pochette[data-name]').count() == 3, 'trois pochettes de personnes')
    check(page.locator('.pe-maybe strong').inner_text() == 'Marie', 'la suggestion « est-ce Marie ? » est proposée')
    page.screenshot(path=str(OUT / 'board-todo.png'))
    thumbs = [b for p_, b in STATE['posts'] if p_.endswith('/thumbs')]
    check(len(thumbs) > 0, f'des vignettes sont demandées par lots ({len(thumbs)} appel(s), ≤ 60 ids : {max(len(t["ids"]) for t in thumbs) if thumbs else 0})')
    # Sélection par clic sur un groupe (halo), puis rangement par la barre d'actions.
    box = page.locator('canvas.pe-board').bounding_box()
    page.locator('.pe-todo button').first.click(); page.wait_for_timeout(300)
    check(page.locator('.pe-tray').is_visible(), 'la barre de sélection apparaît après avoir choisi un groupe')
    n_sel = int(page.inner_text('.pe-tray strong').split()[0]); check(n_sel >= 2, f'le groupe choisi sélectionne {n_sel} visages')
    page.screenshot(path=str(OUT / 'board-selected.png'))
    page.select_option('.pe-tray select', 'Paul'); page.wait_for_timeout(700)
    assigns = [b for p_, b in STATE['posts'] if p_.endswith('/assign')]
    check(len(assigns) == 1 and assigns[0]['name'] == 'Paul' and len(assigns[0]['ids']) == n_sel and assigns[0].get('pubkey') == f'{1:064x}', 'le groupe est rangé dans Paul avec sa clé : ' + str(assigns[-1:] and {k: (v if k != "ids" else len(v)) for k, v in assigns[-1].items()}))
    check(not page.locator('.pe-tray').is_visible(), 'la sélection est vidée après le rangement')
    # Glisser-déposer : un visage isolé vers « Nouvelle personne » ouvre le dialogue de nom.
    page.wait_for_timeout(500)
    node = page.evaluate("""() => { const c = document.querySelector('canvas.pe-board'); return null; }""")
    # Passe en vue « Tous » et vérifie la présence des personnes dessinées.
    page.click('button[data-v=all]'); page.wait_for_timeout(500); page.screenshot(path=str(OUT / 'board-all.png'))
    # Glisser-déposer : un visage de la table vers la pochette « Léa », puis un autre vers « Nouvelle personne ».
    def find_face(skip=0):
        found = 0
        for yy in range(int(box['y']) + 30, int(box['y'] + box['height']) - 30, 9):
            for xx in range(int(box['x']) + 30, int(box['x'] + box['width']) - 30, 9):
                page.mouse.move(xx, yy)
                if page.evaluate("document.querySelector('canvas.pe-board').style.cursor") == 'grab':
                    if found == skip: return xx, yy
                    found += 1
        return None
    pos = find_face(20)
    check(pos is not None, 'un visage est repéré sous le pointeur')
    before = len([b for p_, b in STATE['posts'] if p_.endswith('/assign')])
    lea = page.locator('.pe-pochette[data-name="Léa"]').bounding_box()
    page.mouse.move(*pos); page.mouse.down(); page.mouse.move(pos[0] + 20, pos[1] + 20, steps=4)
    check(page.locator('.pe-ghost').count() == 1, 'une pastille fantôme suit le pointeur pendant le glisser')
    page.mouse.move(lea['x'] + lea['width'] / 2, lea['y'] + lea['height'] / 2, steps=10)
    check(page.locator('.pe-pochette.over').count() == 1, 'la pochette survolée est mise en évidence')
    page.mouse.up(); page.wait_for_timeout(500)
    assigns = [b for p_, b in STATE['posts'] if p_.endswith('/assign')]
    check(len(assigns) == before + 1 and assigns[-1]['name'] == 'Léa' and len(assigns[-1]['ids']) == 1, 'le visage glissé est rangé dans Léa : ' + str(assigns[-1]))
    pos = find_face(5)
    nw = page.locator('.pe-pochette.new').bounding_box()
    page.mouse.move(*pos); page.mouse.down(); page.mouse.move(pos[0] + 20, pos[1] + 20, steps=4); page.mouse.move(nw['x'] + 30, nw['y'] + 15, steps=10); page.mouse.up(); page.wait_for_timeout(300)
    check(page.locator('dialog.pe-name[open]').count() == 1, 'déposer sur « Nouvelle personne » ouvre la demande de nom')
    page.fill('dialog.pe-name input', 'Grand-père'); page.click('dialog.pe-name button[value=ok]'); page.wait_for_timeout(600)
    assigns = [b for p_, b in STATE['posts'] if p_.endswith('/assign')]
    check(assigns[-1]['name'] == 'Grand-père' and not assigns[-1].get('pubkey'), 'le nouveau nom est enregistré')
    page.screenshot(path=str(OUT / 'board-dnd.png'))
    # « Pas à reconnaître » (confirmation acceptée) sur une sélection au lasso.
    page.click('button:has-text("Lasso")')
    page.mouse.move(box['x'] + 40, box['y'] + 40); page.mouse.down(); page.mouse.move(box['x'] + box['width'] - 40, box['y'] + 40, steps=8); page.mouse.move(box['x'] + box['width'] - 40, box['y'] + box['height'] - 40, steps=8); page.mouse.move(box['x'] + 40, box['y'] + box['height'] - 40, steps=8); page.mouse.up(); page.wait_for_timeout(400)
    sel = int(page.inner_text('.pe-tray strong').split()[0]) if page.locator('.pe-tray').is_visible() else 0
    check(sel > 10, f'le lasso sélectionne les visages encerclés ({sel})')
    page.on('dialog', lambda d: d.accept())
    page.click('.pe-tray button:has-text("Pas à reconnaître")'); page.wait_for_timeout(600)
    forgets = [b for p_, b in STATE['posts'] if p_.endswith('/forget')]
    check(len(forgets) == 1 and len(forgets[0]['ids']) == sel, 'les visages sélectionnés sont oubliés (confirmation demandée)')
    # Réplication.
    page.goto(f'http://127.0.0.1:{port}/#/replication'); page.wait_for_selector('.rp-ring'); page.wait_for_timeout(300)
    txt = page.inner_text('main')
    check('27 sur 30' in txt and 'Proche de proche' in txt and 'Maillage P2P' in txt and 'Injoignable' in txt and 'connexion fermée' in txt, 'la réplication montre où, par qui et par quelle voie')
    check('5 sur 8' in txt and 'poppy : 5 fichier' in txt, 'les fichiers fournis par d\'autres stations sont listés')
    page.screenshot(path=str(OUT / 'replication.png'), full_page=True)
    # Configuration coopérative (Capitaine) : secrets en écriture seule, seules les clés modifiées partent.
    page.goto(f'http://127.0.0.1:{port}/#/coop'); page.wait_for_selector('form fieldset'); page.wait_for_timeout(200)
    check(page.locator('main input[type=password]').count() == 1 and page.locator('main input[type=password]').get_attribute('placeholder').startswith('● renseigné'), 'le secret est masqué et jamais affiché')
    check('ZZ_LEGACY' in page.inner_text('main'), 'les clés hors écran sont signalées')
    page.fill('#cf-OCSLUG', 'autre'); page.fill('#cf-TREASURY_PERCENT', '40'); page.wait_for_timeout(100)
    check('additionne' in page.inner_text('.degraded'), 'la somme des parts différente de 100 % est signalée')
    page.fill('#cf-TREASURY_PERCENT', '33.33'); page.click('button:has-text("Publier")'); page.wait_for_timeout(600)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/coop']
    check(sent and sent[-1] == dict(values=dict(OCSLUG='autre')), 'seule la clé modifiée est envoyée : ' + str(sent[-1:]))
    # Options : moteur FaceID et matrice des kinds ; seules les lignes modifiées partent.
    page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('details.fold', state='attached'); page.evaluate("document.querySelectorAll('details.fold').forEach(d => d.open = true)"); page.wait_for_selector('input[name=fo-mode]'); page.wait_for_timeout(200)
    check('Aucun moteur en service' in page.inner_text('main') and '3 photo(s) en attente' in page.inner_text('main'), 'l\'état du moteur FaceID et la file en attente sont visibles')
    page.check('input[name=fo-mode][value=remote]'); page.fill('#fo-url', 'http://brain.local:8190/faces'); page.click('button:text-is("Appliquer")'); page.wait_for_timeout(500)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/face']
    check(sent and sent[-1] == dict(mode='remote', url='http://brain.local:8190/faces'), 'la bascule du moteur part avec son adresse : ' + str(sent[-1:]))
    page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('details.fold', state='attached'); page.evaluate("document.querySelectorAll('details.fold').forEach(d => d.open = true)"); page.wait_for_selector('table select'); page.wait_for_timeout(200)
    page.locator('table tbody tr').nth(1).locator('select').nth(1).select_option('deny')
    page.click('button:has-text("Appliquer la matrice")'); page.wait_for_timeout(500)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/kinds']
    check(sent and sent[-1] == dict(rules={'7': 'read=circle write=deny project=on'}), 'seule la ligne modifiée de la matrice est envoyée : ' + str(sent[-1:]))
    # Mon compte : jeton changé (la session suit), export des clés derrière une confirmation, profil.
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_timeout(200)
    check('npub1test' in page.inner_text('main') and 'wss://s.example' in page.inner_text('main'), 'Mon compte montre l\'identité et les relais annoncés')
    check(page.locator('button:text-is("Exporter mes clés")').is_disabled(), 'l\'export des clés exige une case de confirmation')
    # L'API donne `/dav` : l'écran montre (et copie) l'adresse complète, et le bouton de remplacement n'effraie pas sans raison.
    main_text = page.inner_text('main')
    check(f'http://127.0.0.1:{port}/dav' in main_text and 'celui de la station, jamais remplacé' in main_text, "l'adresse WebDAV est complète et le jeton principal est présenté sans bouton menaçant")
    check(not page.locator('button:text-is("Remplacer le jeton principal")').is_visible(), 'le remplacement du jeton principal est replié')
    page.click('summary:text-is("Remplacer le jeton principal")'); page.click('button:text-is("Remplacer le jeton principal")')  # la confirmation est acceptée par le gestionnaire de dialogue déjà posé
    page.wait_for_selector('text=NOUVEAU-JETON'); page.wait_for_timeout(100)
    import base64 as b64
    page.wait_for_function("localStorage.getItem('basic') !== btoa('alice@example.org:jeton')", timeout=3000); got = b64.b64decode(page.evaluate("localStorage.getItem('basic')")).decode(); check(got == 'alice@example.org:NOUVEAU-JETON', 'la session suit le nouveau jeton : ' + got)
    page.check('#ac-ok'); page.click('button:text-is("Exporter mes clés")'); page.wait_for_selector('textarea[aria-label=nsec]')
    check(page.input_value('textarea[aria-label=nsec]') == 'nsec1secret', 'les clés exportées s\'affichent après confirmation')
    check('[object' not in page.inner_text('main') and page.input_value('textarea[aria-label=dunikey]') == 'sec: x', 'la clé Ğ1 exportée est un champ, pas du texte « [object HTMLLabelElement] »')
    page.fill('#ac-name', 'Alice B'); page.click('button:text-is("Enregistrer")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/account/profile']
    check(sent and sent[-1] == dict(name='Alice B'), 'seul le champ modifié du profil est envoyé : ' + str(sent[-1:]))
    # Multilingue : l'anglais traduit la coque et les écrans, sans rechargement de dictionnaire côté serveur.
    ctx2 = br.new_context(viewport=dict(width=1280, height=820), service_workers='block'); p2 = ctx2.new_page()
    ctx2.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton')); localStorage.setItem('lang', 'en')")
    p2.goto(f'http://127.0.0.1:{port}/#/account'); p2.wait_for_selector('#ac-name'); p2.wait_for_timeout(400)
    nav = p2.inner_text('nav')
    check('Dashboard' in nav and 'My account' in nav and 'Tableau de bord' not in nav, 'la navigation est traduite en anglais : ' + nav.replace('\n', ' | '))
    check('Display name' in p2.inner_text('main') and 'Export my keys' in p2.inner_text('main'), 'l\'écran Mon compte est traduit')
    check(p2.get_attribute('html', 'lang') == 'en', 'l\'attribut lang du document suit')
    ctx2.close()
    # Carnet d'adresses : recherche, index, fiche unifiée, comparaison avec le profil Nostr, « Ma fiche », nouvelle fiche, doublons.
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('.ab-list li button'); page.wait_for_timeout(300)
    names = page.locator('.ab-item strong').all_inner_texts()
    check(names[0] == 'Alice' and 'Moi' in page.locator('.ab-list li').first.inner_text() and names[1:] == ['Léa Martin', 'Paul Durand'], 'le carnet montre « Moi » en tête puis les fiches par ordre alphabétique : ' + str(names))
    check(page.locator('.ab-index a').all_inner_texts() == ['L', 'P'] and page.locator('.chip:has-text("Famille")').count() == 1, "l'index alphabétique et les groupes sont affichés")
    page.fill('.ab-search input', 'lea'); page.wait_for_timeout(500)
    check(any('q=lea' in g for g in STATE.get('gets', [])), 'la recherche part au fil de la frappe : ' + str([g for g in STATE.get('gets', []) if 'q=' in g][-1:]))
    page.click('.ab-item:has-text("Léa Martin")'); page.wait_for_selector('.ab-card'); page.wait_for_timeout(300)
    check(page.input_value('#f-name') == 'Léa Martin' and page.locator('.ab-diff table tbody tr').count() == 2, 'la fiche affiche ses champs et la comparaison avec le profil Nostr')
    page.locator('.ab-diff tr:has-text("Nom") button:has-text("Prendre")').click(); page.wait_for_timeout(400)
    taken = [b for p_, b in STATE['posts'] if p_ == '/api/v1/book/take']
    check(taken and taken[-1] == dict(path='/Contacts/Lea.vcf', keys=['name']), 'on prend UNE valeur du profil, pas tout : ' + str(taken[-1:]))
    page.click('.ab-item:has-text("Alice")'); page.wait_for_selector('.ab-card'); page.wait_for_timeout(300)
    check('Votre profil Nostr et votre fiche sont réunis' in page.inner_text('.ab-head') and page.locator('.ab-diff').count() == 0 and page.locator('button:has-text("Publier et enregistrer")').count() == 1, '« Ma fiche » réunit profil Nostr et fiche, sans comparaison à faire')
    page.fill('#f-about', 'Capitaine du Sud'); page.click('button:has-text("Publier et enregistrer")'); page.wait_for_timeout(400)
    posted = [b for p_, b in STATE['posts'] if p_ == '/api/v1/book/me']
    check(posted and posted[-1]['about'] == 'Capitaine du Sud' and posted[-1]['name'] == 'Alice', 'un seul enregistrement envoie champs publics et privés : ' + str(posted[-1:])[:120])
    page.click('button:has-text("Nouvelle fiche")'); page.wait_for_selector('#f-name'); page.fill('#f-name', 'Nouvelle Amie'); page.click('.ab-actions button:has-text("Enregistrer")'); page.wait_for_timeout(400)
    created = [b for p_, b in STATE['posts'] if p_ == '/api/v1/book/card']
    check(created and created[-1]['path'] is None and created[-1]['fields']['name'] == 'Nouvelle Amie', 'une nouvelle fiche se crée avec son nom : ' + str(created[-1:])[:120])
    page.click('button:has-text("Doublons")'); page.wait_for_selector('button:has-text("Garder celle-ci")'); page.wait_for_timeout(200)
    check('même téléphone' in page.inner_text('.ab-detail'), 'les doublons probables sont proposés avec leur raison')
    page.screenshot(path=str(OUT / 'contacts-book.png'))
    # Contacts : l'annuaire de synchronisation (onglet « Synchroniser et inviter »), la liaison manuelle et la création d'une fiche depuis une personne.
    def open_sync():
        page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_timeout(100); page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('button:has-text("Synchroniser et inviter")'); page.click('button:has-text("Synchroniser et inviter")'); page.wait_for_selector('table')
    open_sync(); page.wait_for_timeout(200)
    txt = page.inner_text('main')
    check('Léa Martin' in txt and 'Proches réciproques' in txt and 'sans lien' in txt and 'https://s.example/dav/' in txt, 'l\'annuaire montre les fiches, leur lien MULTIPASS et l\'adresse CardDAV')
    page.locator('table tbody tr').nth(1).locator('input').fill('bea@example.org'); page.locator('table tbody tr').nth(1).locator('button:text-is("Relier")').click(); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/contacts/link']
    check(sent and sent[-1] == dict(path='/Contacts/Paul.vcf', target='bea@example.org'), 'la liaison manuelle part avec la fiche et la cible : ' + str(sent[-1:]))
    page.click('button:text-is("Créer la fiche")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/contacts/from-person']
    check(sent and sent[-1] == dict(name='Nina'), 'la fiche se crée depuis une personne reconnue')
    # Invitations : le lien se crée depuis une fiche (avec son QR), la liste montre les états, le lien se retire.
    open_sync(); page.wait_for_timeout(200)
    check(page.locator('table tbody tr:has-text("Léa Martin") button:text-is("Inviter")').count() == 0, 'une fiche déjà reliée à un MULTIPASS n\'a pas de bouton Inviter')
    page.locator('table tbody tr:has-text("Paul") button:text-is("Inviter")').click(); page.wait_for_selector('img[alt^="QR code du lien"]'); page.wait_for_timeout(200)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/invites']
    check(sent and sent[-1] == dict(person='Paul', label='Paul', contact_path='/Contacts/Paul.vcf'), 'l\'invitation part avec la personne et sa fiche : ' + str(sent[-1:]))
    check(page.input_value('input[readonly]') == 'https://s.example/i/' + TOKEN, 'le lien d\'invitation est affiché, prêt à copier')
    page.screenshot(path=str(OUT / 'contacts-invite.png'))
    txt = page.inner_text('main')
    check('a rejoint' in txt and 'ouverte' in txt, 'la liste des invitations montre leur état : a rejoint / ouverte')
    page.locator('table tbody tr:has-text("Zoé") button:text-is("Retirer")').click(); page.wait_for_timeout(300)
    check(STATE['deleted'] == ['/api/v1/invites/' + '2' * 32], 'retirer supprime le lien : ' + str(STATE['deleted']))
    open_sync()
    page.locator('main').get_by_text('Nina').locator('xpath=..').locator('button:text-is("Inviter")').click(); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/contacts/from-person']
    check(sent and sent[-1] == dict(name='Nina'), 'inviter une personne sans fiche crée d\'abord sa fiche')
    # La page publique du lien, en français puis en anglais, sans compte.
    for loc, title, btn in (('fr-FR', 'alice vous envoie des photos', 'Recevoir mes photos'), ('en-US', 'alice sent you photos', 'Get my photos')):
        c3 = br.new_context(viewport=dict(width=390, height=800), locale=loc, service_workers='block'); p3 = c3.new_page()
        p3.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); p3.on('pageerror', lambda e: errors.append(str(e)))
        p3.goto(f'http://127.0.0.1:{port}/i/{TOKEN}'); p3.wait_for_selector('#f:not([hidden])'); p3.wait_for_timeout(300)
        check(p3.inner_text('#title') == title, f'la page du lien ({loc}) annonce l\'expéditeur : ' + p3.inner_text('#title'))
        check(p3.locator('#thumbs img').count() == 2 and p3.evaluate("[...document.images].every(i => i.naturalWidth > 0)"), 'deux vignettes floutées chargées')
        check(p3.inner_text('#go') == btn, 'le bouton est traduit : ' + p3.inner_text('#go'))
        if loc == 'fr-FR':
            p3.fill('#email', 'lea@example.org'); p3.click('#go'); p3.wait_for_selector('#done:not([hidden])')
            sent = [b for p_, b in STATE['posts'] if p_.endswith('/claim')]
            check(sent and sent[-1] == dict(email='lea@example.org', lang='fr'), 'la réclamation envoie l\'adresse saisie : ' + str(sent[-1:]))
            check('4 fichier(s) de alice' in p3.inner_text('#got') and 'nsec1secret' in p3.inner_text('#card'), 'le compte créé et ses secrets sont montrés une fois')
            p3.screenshot(path=str(OUT / 'invite-page.png'))
        c3.close()
    # Envoi par la messagerie de l'expéditeur : liens mailto / sms / WhatsApp préremplis (la station n'envoie jamais de courrier).
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('table')
    page.locator('table tbody tr:has-text("Paul") button:text-is("Inviter")').click(); page.wait_for_selector('main a.btn:text-is("WhatsApp")')
    page.wait_for_timeout(600)  # la liste se redessine une seconde fois (rechargement des invitations) : on attend qu'elle se stabilise
    page.wait_for_function("document.querySelectorAll('main a.btn').length >= 3")
    hrefs = dict(page.evaluate("[...document.querySelectorAll('main a.btn')].map(a => [a.textContent, a.getAttribute('href')])"))
    url_q = 'https%3A%2F%2Fs.example%2Fi%2F' + TOKEN
    check(hrefs.get('Email', '').startswith('mailto:?subject=') and url_q in hrefs.get('Email', ''), 'le lien mailto est prérempli avec l\'invitation : ' + str(hrefs)[:300])
    check(hrefs.get('SMS', '').startswith('sms:?&body=') and url_q in hrefs.get('SMS', ''), 'le lien sms est prérempli')
    check(hrefs.get('WhatsApp', '').startswith('https://wa.me/?text=') and url_q in hrefs.get('WhatsApp', ''), 'le lien WhatsApp est prérempli')
    # Un lien qui ne sert plus : le code stable est traduit dans la langue du navigateur.
    for loc, expect in (('fr-FR', 'Ce lien a expiré'), ('en-US', 'This link has expired'), ('es-ES', 'Este enlace ha caducado')):
        c6 = br.new_context(locale=loc, service_workers='block'); p6 = c6.new_page()
        p6.goto(f'http://127.0.0.1:{port}/i/' + 'b2' * 16); p6.wait_for_timeout(500)
        check(expect in p6.inner_text('#lead'), f'le code invite_expired est traduit ({loc}) : ' + p6.inner_text('#lead')[:60])
        c6.close()
    # Déjà connecté : « accepter avec mon compte », sans formulaire ni nouveau compte.
    c7 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block'); p7 = c7.new_page()
    c7.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    p7.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); p7.on('pageerror', lambda e: errors.append(str(e)))
    p7.goto(f'http://127.0.0.1:{port}/i/{TOKEN}'); p7.wait_for_selector('#acc:not([hidden])')
    check('Bonjour Léa, ajouter ces 4 fichier(s) à votre compte' in p7.inner_text('#acc-t') and p7.is_hidden('#f'), 'une session ouverte propose d\'accepter, sans formulaire : ' + p7.inner_text('#acc-t'))
    p7.screenshot(path=str(OUT / 'invite-accept.png'))
    before = len([1 for p_, _ in STATE['posts'] if p_.endswith('/accept')])
    p7.click('#acc-go'); p7.wait_for_selector('#acc-done:not([hidden])')
    check(len([1 for p_, _ in STATE['posts'] if p_.endswith('/accept')]) == before + 1 and '4 fichier(s) de alice ont été ajoutés' in p7.inner_text('#acc-done-t'), 'l\'acceptation part avec la session et confirme : ' + p7.inner_text('#acc-done-t'))
    c7.close()
    # « Utiliser un autre compte » ramène au formulaire.
    c8 = br.new_context(locale='fr-FR', service_workers='block'); p8 = c8.new_page()
    c8.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    p8.goto(f'http://127.0.0.1:{port}/i/{TOKEN}'); p8.wait_for_selector('#acc:not([hidden])'); p8.click('#acc-other')
    check(p8.is_visible('#f') and p8.is_hidden('#acc'), 'l\'autre compte rouvre le formulaire d\'inscription')
    c8.close()
    # Adresse déjà hébergée (Safari, session dans la PWA) : le PASS saisi sur place suffit pour accepter.
    c9 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block'); p9 = c9.new_page()
    p9.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text and '409' not in m.text else None); p9.on('pageerror', lambda e: errors.append(str(e)))
    p9.goto(f'http://127.0.0.1:{port}/i/{TOKEN}'); p9.wait_for_selector('#f:not([hidden])')
    p9.fill('#email', 'taken@example.org'); p9.click('#go'); p9.wait_for_selector('#pw:not([hidden])')
    check(p9.is_hidden('#f') and p9.is_hidden('#err'), 'adresse déjà prise : le formulaire cède la place au PASS')
    p9.fill('#pw-pass', '000000'); p9.click('#pw-go'); p9.wait_for_timeout(400)
    check('incorrect' in p9.inner_text('#pw-e'), 'un PASS faux est signalé : ' + p9.inner_text('#pw-e'))
    p9.fill('#pw-pass', '123456'); p9.click('#pw-go'); p9.wait_for_selector('#acc:not([hidden])')
    p9.click('#acc-go'); p9.wait_for_selector('#acc-done:not([hidden])')
    check(p9.evaluate("localStorage.getItem('basic')") is None, 'la session obtenue par PASS n\'est pas enregistrée dans ce navigateur')
    c9.close()
    # Compte sur une autre station : l'invitation y envoie, le jeton dans le fragment ; la page de là-bas retire le jeton de l'adresse,
    # demande le PASS (pas de session dans ce navigateur) puis envoie origine et jeton à SA station.
    c10 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block'); p10 = c10.new_page()
    p10.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text else None); p10.on('pageerror', lambda e: errors.append(str(e)))
    p10.goto(f'http://localhost:{port}/i/{TOKEN}'); p10.wait_for_selector('#f:not([hidden])')
    p10.click('#away-open'); p10.fill('#away-host', f'http://localhost:{port}'); p10.click('#away-go'); p10.wait_for_timeout(300)
    check('adresse de votre station' in p10.inner_text('#away-e').lower(), 'sa propre station est refusée : ' + p10.inner_text('#away-e'))
    p10.fill('#away-host', f'http://127.0.0.1:{port}'); p10.click('#away-go'); p10.wait_for_selector('#login:not([hidden])')
    check(p10.url == f'http://127.0.0.1:{port}/claim', 'le jeton est retiré de l\'adresse dès la lecture : ' + p10.url)
    p10.fill('#u', 'alice@example.org'); p10.fill('#p', '123456'); p10.click('#login-go'); p10.wait_for_selector('#ask:not([hidden])')
    check('alice@example.org' in p10.inner_text('#lead'), 'le compte qui recevra est nommé : ' + p10.inner_text('#lead'))
    p10.click('#yes'); p10.wait_for_selector('#done:not([hidden])')
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/invites/remote-accept']
    check(sent and sent[-1] == dict(origin=f'http://localhost:{port}', token=TOKEN), 'sa station reçoit origine et jeton : ' + str(sent[-1:]))
    check('alice' in p10.inner_text('#done-t') and p10.evaluate("localStorage.getItem('basic')") is None, 'accepté, sans session enregistrée : ' + p10.inner_text('#done-t'))
    c10.close()
    # Connexion par code PASS : la station rend le jeton ; `window.nostr` délègue la signature à la station.
    ctx4 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block'); p4 = ctx4.new_page()
    p4.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text else None); p4.on('pageerror', lambda e: errors.append(str(e)))
    p4.goto(f'http://127.0.0.1:{port}/#/account'); p4.wait_for_selector('#login-f', state='visible')
    p4.fill('input[name=u]', 'alice@example.org'); p4.fill('input[name=p]', '000000'); p4.click('#login-f button[value=ok]'); p4.wait_for_timeout(400)
    check('4 essai(s) restant(s)' in p4.inner_text('#login-e'), 'un PASS faux affiche les essais restants : ' + p4.inner_text('#login-e'))
    p4.fill('input[name=p]', '123456'); p4.click('#login-f button[value=ok]'); p4.wait_for_selector('#ac-name'); p4.wait_for_timeout(300)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/account/unlock']
    check(sent and sent[-1] == {'email': 'alice@example.org', 'pass': '123456'}, 'le PASS part vers la station : ' + str(sent[-1:]))
    check(p4.evaluate("localStorage.getItem('basic') || localStorage.getItem('basic')") is not None, 'la session est conservée')
    check(p4.evaluate("!!window.nostr && window.nostr.astro === true"), 'window.nostr est fourni par la PWA')
    check(p4.evaluate("window.nostr.getPublicKey()") == 'aa' * 32, 'getPublicKey rend la clé du compte')
    ev = p4.evaluate("window.nostr.signEvent({ kind: 1, content: 'salut', tags: [] })")
    check(ev['sig'] == '22' * 64 and ev['content'] == 'salut', 'signEvent est signé par la station')
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/account/sign']
    check(sent and sent[-1] == dict(kind=1, content='salut', tags=[]), 'seul l\'événement non signé part (jamais de clé) : ' + str(sent[-1:]))
    ctx4.close()
    # Inscription sans email : la case remplace l'adresse par un alias facultatif.
    p5 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block').new_page()
    p5.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); p5.on('pageerror', lambda e: errors.append(str(e)))
    p5.goto(f'http://127.0.0.1:{port}/register'); p5.wait_for_selector('#f:not([hidden])')
    p5.check('#guest'); p5.fill('#alias', 'léa'); p5.click('#go'); p5.wait_for_selector('#done:not([hidden])')
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/register']
    check(sent and sent[-1] == dict(guest=True, alias='léa', lang='fr'), 'l\'inscription sans email envoie un alias, pas d\'adresse : ' + str(sent[-1:]))
    check('lea-x7k2p9@uplanet.local' in p5.inner_text('#card') and '123456' in p5.inner_text('#card'), 'l\'adresse générée et le PASS à 6 chiffres sont remis')
    check(p5.is_visible('#qr svg') and CARD in p5.inner_text('#card') and p5.is_visible('#print'), 'la clé de secours est remise en texte et en QR imprimable')
    # Rétablir son compte sur cette station depuis la carte : le champ est vidé dès la réponse, le nouveau PASS est remis.
    p6 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block').new_page()
    p6.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); p6.on('pageerror', lambda e: errors.append(str(e)))
    p6.goto(f'http://127.0.0.1:{port}/restore'); p6.wait_for_selector('#f:not([hidden])')
    check(not p6.is_visible('#scan'), 'sans lecteur de QR du navigateur, seul le collage est proposé')
    p6.fill('#card', CARD); p6.click('#go'); p6.wait_for_selector('#done:not([hidden])')
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/restore']
    check(sent and sent[-1] == dict(card=CARD, lang='fr'), 'seules les parts saisies partent, vers cette station : ' + str(sent[-1:]))
    check('654321' in p6.inner_text('#out') and 'alice@example.org' in p6.inner_text('#out') and p6.input_value('#card') == '', 'le nouveau PASS est remis et la carte effacée du champ')
    # Compte invité : bandeau honnête (pas de fonction promise qui n'existe pas).
    ACCOUNT['guest'] = True
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_timeout(200)
    check('Compte invité' in page.inner_text('main') and 'clé de secours' in page.inner_text('main'), 'un compte invité voit le bandeau de récupération')
    ACCOUNT['guest'] = False
    # Thème sombre.
    page.emulate_media(color_scheme='dark'); page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(800); page.screenshot(path=str(OUT / 'board-dark.png'))
    # Quitter un écran lent avant la fin de son chargement ne doit pas faire apparaître son erreur dans l'écran suivant.
    page.route('**/api/v1/faces/board', lambda route: (page.wait_for_timeout(2500), route.continue_()))
    page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_timeout(300)
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('main >> .ab-search'); page.wait_for_timeout(4500)
    txt = page.inner_text('main')
    check('Nouvelle fiche' in txt and 'Cannot' not in txt and 'null' not in txt and 'Table de tri' not in txt, "l'écran quitté en cours de chargement n'écrit rien dans le suivant")
    page.unroute('**/api/v1/faces/board')
    # Appareils : un jeton WebDAV par téléphone, confiné à un dossier.
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#dv-label'); page.wait_for_timeout(300)
    txt = page.inner_text('main')
    check('Téléphone de Léa' in txt and '/Photos/Léa' in txt and 'Envoi seul' in txt and 'Jamais utilisé' in txt, "l'appareil est listé avec son dossier, ses droits et son dernier usage")
    page.fill('#dv-label', 'Tablette'); page.select_option('#dv-mode', 'readwrite'); page.click('button:text-is("Ajouter cet appareil")'); page.wait_for_selector('text=ajouté'); page.wait_for_timeout(300)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/devices']
    check(len(sent) == 1 and sent[0] == dict(label='Tablette', root='/Photos/Tablette', mode='readwrite'), "le dossier par défaut est /Photos/<nom> et les droits choisis partent")
    check(page.input_value('input[aria-label=Jeton]') == 'dv_' + 'ab' * 16 and 'ne sera plus jamais affiché' in page.inner_text('main'), 'le jeton est montré une fois, avec le serveur et le dossier')
    page.click('button:text-is("Révoquer")'); page.wait_for_timeout(400)
    check(any(p_ == '/api/v1/devices/revoke' and b_ == {'id': 'd1'} for p_, b_ in STATE['posts']), "« Révoquer » révoque le bon appareil")
    # Sources : reprise depuis un autre serveur WebDAV, lancée depuis l'interface.
    page.goto(f'http://127.0.0.1:{port}/#/cloud?m=sources'); page.wait_for_selector('#src-url'); page.wait_for_timeout(300)
    txt = page.inner_text('main')
    check('Mon Nextcloud' in txt and 'Photos' in txt and '/Imports/Mon Nextcloud' in txt, 'la source est listée avec son dossier distant et son dépôt')
    check(page.evaluate("document.querySelector('progress')?.value") == 40 and page.get_by_role('button', name='Arrêter').is_visible(), 'une reprise en cours montre sa progression et un bouton Arrêter')
    check('fred@' not in txt and 'mot de passe' not in txt.lower().replace("mot de passe d'application", ''), 'aucun secret affiché')
    page.fill('#src-url', 'https://nc2.example/remote.php/dav/files/fred'); page.fill('#src-user', 'fred'); page.fill('#src-pass', 'secret-app'); page.fill('#src-rp', 'Docs')
    page.click('button:text-is("Vérifier et ajouter")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/pull']
    check(len(sent) == 1 and sent[0]['url'].startswith('https://nc2.example') and sent[0]['password'] == 'secret-app' and sent[0]['remote_path'] == 'Docs' and 'allow_private' not in sent[0], "le formulaire envoie l'adresse et le mot de passe, jamais d'adresse privée admise")
    page.click('button:text-is("Arrêter")'); page.wait_for_timeout(300)
    check(any(p_ == '/api/v1/pull/stop' for p_, _ in STATE['posts']), "« Arrêter » demande l'arrêt de la reprise")
    # Ranger : le uCloud par type, avec aperçu, lancement et annulation.
    page.goto(f'http://127.0.0.1:{port}/#/cloud?m=organize'); page.wait_for_selector('#og-scope'); page.wait_for_timeout(300)
    page.click('button:text-is("Voir ce qui serait rangé")'); page.wait_for_selector('text=seraient rangés'); txt = page.inner_text('main')
    check('7 fichier(s) seraient rangés' in txt and 'Images' in txt and '/Images/p.jpg' in txt and '4 protégé(s)' in txt, "l'aperçu dit combien, par type, avec des exemples et ce qui reste en place")
    page.uncheck('input[name=archives]'); page.select_option('#og-date', 'month'); page.click('button:text-is("Ranger maintenant")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/organize/run']
    check(len(sent) == 1 and sent[0]['by_date'] == 'month' and 'archives' not in sent[0]['kinds'] and len(sent[0]['kinds']) == 4 and sent[0]['scope'] == '/', 'le rangement part avec le dossier, les types et les sous-dossiers par date choisis')
    page.click('button:text-is("Annuler ce rangement")'); page.wait_for_timeout(400)
    check(any(p_ == '/api/v1/organize/undo' and b_ == {'run': 'r1'} for p_, b_ in STATE['posts']), "« Annuler ce rangement » demande l'annulation du bon passage")
    # Plans : liste, redémarrage nécessaire, ajout d'une UPlanet neuve (la clé créée est montrée pour être copiée).
    page.goto(f'http://127.0.0.1:{port}/#/planes'); page.wait_for_selector('input[aria-label="Nom du plan"]'); page.wait_for_timeout(500)
    check(page.locator('table tbody tr').count() == 2 and 'zen1' in page.inner_text('main'), 'les plans de la station sont listés')
    check(page.locator('#plane option').count() == 2 and page.input_value('#plane') == 'origin', "le sélecteur de plan de l'en-tête propose ORIGIN et zen1")
    check('Redémarrez' in page.inner_text('main'), 'le redémarrage nécessaire est dit')
    page.fill('input[aria-label="Nom du plan"]', 'zen2'); page.click('form button:has-text("Ajouter")'); page.wait_for_selector('textarea[readonly]'); page.wait_for_timeout(300)
    check('/key/swarm/psk/1.0.0/' in page.input_value('textarea[readonly]') and 'zen2' in [b['name'] for p_, b in STATE['posts'] if p_ == '/api/v1/station/planes'], 'fonder une UPlanet montre la swarm.key créée, à copier')
    # BRO : masqué tant que le MULTIPASS n'a déclaré ni Ollama ni clé d'API ; la déclaration (Mon compte) le fait apparaître.
    STATE['ai'] = False
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.reload(); page.wait_for_selector('#ai-url'); page.wait_for_timeout(300)
    check(page.locator('nav a[data-r=bro]').is_hidden(), "sans IA déclarée, l'onglet BRO n'apparaît pas")
    page.goto(f'http://127.0.0.1:{port}/#/bro'); page.wait_for_selector('#ai-url'); page.wait_for_timeout(300)
    check(page.evaluate('location.hash') == '#/account', "ouvrir #/bro sans IA renvoie vers Mon compte")
    page.fill('#ai-url', 'http://127.0.0.1:11434'); page.click('section:has(#ai-url) button:has-text("Enregistrer")'); page.wait_for_selector('#ai-url'); page.wait_for_timeout(500)
    saved = [b for p_, b in STATE['posts'] if p_ == '/api/v1/me/ai']
    check(saved and saved[-1]['ollama_url'] == 'http://127.0.0.1:11434' and 'api_key' not in saved[-1], "l'adresse d'Ollama est enregistrée, sans clé inventée")
    check(page.locator('nav a[data-r=bro]').is_visible(), "une fois l'IA déclarée, l'onglet BRO apparaît")
    # Bibliothèque : fil du temps jour par jour, carte du monde hors ligne avec ses groupes, doublons sans options superflues.
    page.goto(f'http://127.0.0.1:{port}/#/library'); page.wait_for_selector('#lib-tab-time'); page.wait_for_timeout(700)
    check(page.locator('.stream h3.sticky').count() >= 1 and page.locator('.stream h4').count() >= 2, "l'affichage sur le temps groupe les photos par jour : " + str(page.locator('.stream h4').all_inner_texts()[:2]))
    page.click('#lib-tab-map'); page.wait_for_selector('canvas.map'); page.wait_for_timeout(600)
    lit = page.evaluate("(() => { const c = document.querySelector('canvas.map'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data; const seen = new Set(); for (let i = 0; i < d.length; i += 4 * 97) seen.add(d[i] + ',' + d[i+1] + ',' + d[i+2]); return seen.size; })()")
    check(lit >= 3, f'la carte du monde est dessinée (fond, continents, groupes : {lit} couleurs)')
    check('16 photo(s) géolocalisée(s) sur 40' in page.inner_text('main'), 'le compte de photos géolocalisées est affiché')
    check(page.locator('.histo .bar-m').count() == 2, "l'histogramme propose un bâton par mois")
    box = page.locator('canvas.map').bounding_box()
    page.mouse.click(box['x'] + box['width'] * (1.44 + 180) / 360 - 0, box['y'] + box['height'] / 2)
    page.screenshot(path=str(OUT / 'library-map.png'))
    page.click('.histo .bar-m >> nth=1'); page.wait_for_timeout(500)
    check(page.input_value('input[type=date] >> nth=-2') != '' or page.locator('.histo .bar-m.on').count() == 1, 'un clic sur un mois choisit la période')
    # Sablier pendant le chargement de la table de tri : visible tant que la station calcule (réponse retardée), caché ensuite.
    STATE['delay'] = 1.5
    page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_selector('.pe-busy'); page.wait_for_timeout(300)
    check(page.locator('.pe-busy').is_visible() and page.get_attribute('.pe-stage', 'aria-busy') == 'true', 'le sablier est affiché pendant le chargement de la table de tri')
    page.wait_for_timeout(2200)
    check(not page.locator('.pe-busy').is_visible(), 'le sablier disparaît quand la table est prête')
    STATE['delay'] = 0
    # Table de tri : supprimer les photos des visages sélectionnés (confirmation avec le compte exact).
    page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(700)
    page.locator('.pe-todo button').first.click(); page.wait_for_timeout(300)
    page.once('dialog', lambda d: STATE.setdefault('dialogs', []).append(d.message))  # la confirmation est acceptée par le gestionnaire général
    page.click('.pe-tray button:has-text("Supprimer les photos")'); page.wait_for_timeout(600)
    wiped = [b for p_, b in STATE['posts'] if p_.endswith('/faces/delete-photos')]
    check(len(wiped) == 1 and len(wiped[0]['ids']) >= 1 and 'irréversible' in STATE['dialogs'][0], 'supprimer les photos : confirmation puis appel avec les visages choisis')
    # Extension Nostr (NIP-07) : deux requêtes identiques dans la même seconde ne doivent pas produire le même événement
    # (la station refuse un événement déjà vu : 401 et fenêtre de connexion en pleine table de tri).
    ext = br.new_context(viewport=dict(width=1100, height=800), service_workers='block')
    ext.add_init_script("localStorage.setItem('nip07', '1'); window.nostr = { getPublicKey: async () => 'aa'.repeat(32), signEvent: async ev => { (window.__signed ||= []).push(ev); return { ...ev, id: 'bb'.repeat(32), pubkey: 'aa'.repeat(32), sig: 'cc'.repeat(64) }; } };")
    pe = ext.new_page(); pe.goto(f'http://127.0.0.1:{port}/#/account'); pe.wait_for_selector('#ac-name'); pe.wait_for_timeout(300)
    signed = pe.evaluate('window.__signed || []'); nonces = [t[1] for ev in signed for t in ev['tags'] if t[0] == 'nonce']
    check(len(signed) >= 2 and len(nonces) == len(signed) and len(set(nonces)) == len(nonces) and all(len(n) >= 16 for n in nonces), f"chaque événement signé par l'extension porte un nonce unique ({len(signed)} événements)")
    ext.close()
    # Style (Aurore par défaut, Sobre, Kraft) et mode clair/sombre : choisis dans l'en-tête, retenus par l'appareil.
    page.emulate_media(color_scheme='light'); page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#style')
    check(page.evaluate("document.documentElement.dataset.style") == 'aurore', 'le style par défaut est Aurore')
    page.select_option('#style', 'kraft')
    check(page.evaluate("[document.documentElement.dataset.style, localStorage.getItem('style')]") == ['kraft', 'kraft'], 'le style choisi est appliqué et retenu')
    page.reload(); page.wait_for_selector('#style'); page.wait_for_timeout(200)
    check(page.evaluate("document.documentElement.dataset.style") == 'kraft' and page.input_value('#style') == 'kraft', 'le style survit au rechargement')
    page.click('#mode')
    check(page.evaluate("document.documentElement.dataset.mode") == 'light', 'le bouton de mode passe de automatique à clair')
    page.click('#mode')
    check(page.evaluate("[document.documentElement.dataset.mode, getComputedStyle(document.body).colorScheme]") == ['dark', 'dark'], 'puis à sombre : la page suit')
    page.set_viewport_size(dict(width=1280, height=800)); page.wait_for_timeout(200)
    check(page.evaluate("(() => { const n = [...document.querySelectorAll('header nav a')].filter(a => !a.hidden).map(a => a.getBoundingClientRect().top); return Math.max(...n) - Math.min(...n) < 8; })()"), 'les écrans tiennent sur une seule ligne à 1280 px')
    page.select_option('#style', 'sobre'); page.click('#mode')
    check(page.evaluate("[document.documentElement.dataset.style, document.documentElement.dataset.mode]") == ['sobre', None], 'Sobre et retour au mode automatique')
    page.evaluate("localStorage.removeItem('style'); localStorage.removeItem('mode')")
    # Mobile.
    page.set_viewport_size(dict(width=390, height=800)); page.wait_for_timeout(300); page.screenshot(path=str(OUT / 'board-mobile.png'))
    br.close()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print('RESULT', 'FAIL' if failures else 'OK')
sys.exit(1 if failures else 0)
