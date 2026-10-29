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
ACCOUNT = dict(hex='aa' * 32, npub='npub1test', email='alice@example.org', role='multipass', g1pub='G1TEST', home_station='Qm:ab', lang='fr', langs=['fr', 'en', 'es'], profile=dict(name='Alice', about='Bonjour', picture=None),
               dav=dict(url='https://s.example/dav', login='alice@example.org'), token_generation=0, keys_exportable=True, has_g1_key=True, relays=[dict(url='wss://s.example', read=True, write=True)])
CONTACTS = dict(contacts=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=['lea@example.org'], pubkey='ab' * 32, via='vcard', g1pub=None, warning=None, hosted_here=None, friends=True, faces=12, faces_linked=12),
                          dict(path='/Contacts/Paul.vcf', name='Paul', emails=[], pubkey=None, via=None, g1pub=None, warning=None, hosted_here=None, friends=False, faces=3, faces_linked=0)],
                people_without_contact=[dict(name='Nina', faces=4)], carddav=dict(url='https://s.example/dav/addressbooks/alice%40example.org/contacts/', server='https://s.example/dav/', login='alice@example.org'))
INVITES = dict(invites=[dict(token='1' * 32, label='Marc', email=None, contact_path=None, count=5, state='claimed', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=1790000200, url='https://s.example/i/' + '1' * 32),
                        dict(token='2' * 32, label='Zoé', email=None, contact_path=None, count=3, state='opened', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=None, url='https://s.example/i/' + '2' * 32)])
TOKEN = 'a1' * 16
QR = '<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>'
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
            if not (self.headers.get('Authorization') or '').startswith('Basic '): return self.send(401, dict(error='authentification requise'))
            return self.send(200, dict(hex='aa' * 32, email='alice@example.org', captain=True))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/account': return self.send(200, ACCOUNT)
        if p == '/api/v1/contacts': return self.send(200, CONTACTS)
        if p.startswith('/api/v1/station/admin/'): return self.send(200, ADMIN[p.rsplit('/', 1)[1]])
        if p == '/api/v1/faces/board':
            return self.send(200, dict(enabled=True, dav='/dav', faces=STATE['faces'], people=people(), unknown=sum(1 for f in STATE['faces'] if not f['named']), groups_skipped=False, truncated=False))
        if p == '/api/v1/replication': return self.send(200, REPL)
        if p == '/api/v1/activity': return self.send(200, b': ok\n\n', 'text/event-stream')
        self.send(404, dict(error='simulé : ' + p))
    def do_DELETE(self):
        STATE['deleted'].append(self.path); self.send(200, dict(revoked=True))
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); body = json.loads(self.rfile.read(n) or b'{}'); p = self.path
        STATE['posts'].append((p, body))
        if p == '/api/v1/faces/thumbs': return self.send(200, dict(thumbs={str(i): thumb(i) for i in body['ids']}, failed=[]))
        if p == '/api/v1/faces/assign':
            for f in STATE['faces']:
                if f['id'] in body['ids']: f.update(name=body['name'], pubkey=body.get('pubkey'), named=True, group_id=None, group_size=None, suggest=None)
            return self.send(200, dict(assigned=len(body['ids']), name=body['name']))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p.startswith('/api/v1/station/admin/'): return self.send(200, ADMIN[p.rsplit('/', 1)[1]])
        if p == '/api/v1/register': return self.send(200, dict(email='lea-x7k2p9@uplanet.local', token='JT', pass_='123456', salt='S', pepper='P', nsec='nsec1x', npub='npub1x', hex='cd' * 32, g1pub='G', nostrns='/ipns/k', constellation='ORIGIN', lang='fr', guest=True, **{'pass': '123456'}))
        if p == '/api/v1/invites': return self.send(200, dict(token=TOKEN, count=3, label=body.get('label'), expires_at=1791000000, url='https://s.example/i/' + TOKEN, qr_svg=QR))
        if p == f'/api/v1/invite/{TOKEN}/accept': return self.send(200, dict(**{'from': 'alice'}, shared=4, warnings=[]))
        if p == f'/api/v1/invite/{TOKEN}/claim':
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
    page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('input[name=fo-mode]'); page.wait_for_timeout(200)
    check('Aucun moteur en service' in page.inner_text('main') and '3 photo(s) en attente' in page.inner_text('main'), 'l\'état du moteur FaceID et la file en attente sont visibles')
    page.check('input[name=fo-mode][value=remote]'); page.fill('#fo-url', 'http://brain.local:8190/faces'); page.click('button:text-is("Appliquer")'); page.wait_for_timeout(500)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/face']
    check(sent and sent[-1] == dict(mode='remote', url='http://brain.local:8190/faces'), 'la bascule du moteur part avec son adresse : ' + str(sent[-1:]))
    page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('table select'); page.wait_for_timeout(200)
    page.locator('table tbody tr').nth(1).locator('select').nth(1).select_option('deny')
    page.click('button:has-text("Appliquer la matrice")'); page.wait_for_timeout(500)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/kinds']
    check(sent and sent[-1] == dict(rules={'7': 'read=circle write=deny project=on'}), 'seule la ligne modifiée de la matrice est envoyée : ' + str(sent[-1:]))
    # Mon compte : jeton changé (la session suit), export des clés derrière une confirmation, profil.
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_timeout(200)
    check('npub1test' in page.inner_text('main') and 'wss://s.example' in page.inner_text('main'), 'Mon compte montre l\'identité et les relais annoncés')
    check(page.locator('button:text-is("Exporter mes clés")').is_disabled(), 'l\'export des clés exige une case de confirmation')
    page.click('button:text-is("Changer le jeton")')  # la confirmation est acceptée par le gestionnaire de dialogue déjà posé; page.wait_for_selector('text=NOUVEAU-JETON'); page.wait_for_timeout(100)
    import base64 as b64
    page.wait_for_function("localStorage.getItem('basic') !== btoa('alice@example.org:jeton')", timeout=3000); got = b64.b64decode(page.evaluate("localStorage.getItem('basic')")).decode(); check(got == 'alice@example.org:NOUVEAU-JETON', 'la session suit le nouveau jeton : ' + got)
    page.check('#ac-ok'); page.click('button:text-is("Exporter mes clés")'); page.wait_for_selector('textarea[aria-label=nsec]')
    check(page.input_value('textarea[aria-label=nsec]') == 'nsec1secret', 'les clés exportées s\'affichent après confirmation')
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
    # Contacts : l'annuaire, la liaison manuelle et la création d'une fiche depuis une personne.
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('table'); page.wait_for_timeout(200)
    txt = page.inner_text('main')
    check('Léa Martin' in txt and 'Proches réciproques' in txt and 'sans lien' in txt and 'https://s.example/dav/' in txt, 'l\'annuaire montre les fiches, leur lien MULTIPASS et l\'adresse CardDAV')
    page.locator('table tbody tr').nth(1).locator('input').fill('bea@example.org'); page.locator('table tbody tr').nth(1).locator('button:text-is("Relier")').click(); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/contacts/link']
    check(sent and sent[-1] == dict(path='/Contacts/Paul.vcf', target='bea@example.org'), 'la liaison manuelle part avec la fiche et la cible : ' + str(sent[-1:]))
    page.click('button:text-is("Créer la fiche")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/contacts/from-person']
    check(sent and sent[-1] == dict(name='Nina'), 'la fiche se crée depuis une personne reconnue')
    # Invitations : le lien se crée depuis une fiche (avec son QR), la liste montre les états, le lien se retire.
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('table'); page.wait_for_timeout(200)
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
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('table')
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
    page.locator('table tbody tr:has-text("Paul") button:text-is("Inviter")').click(); page.wait_for_selector('a:text-is("WhatsApp")')
    hrefs = {a.text_content(): a.get_attribute('href') for a in page.locator('main a.btn').all()}
    url_q = 'https%3A%2F%2Fs.example%2Fi%2F' + TOKEN
    check(hrefs.get('Email', '').startswith('mailto:?subject=') and url_q in hrefs.get('Email', ''), 'le lien mailto est prérempli avec l\'invitation : ' + hrefs.get('Email', '')[:90])
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
    # Compte invité : bandeau honnête (pas de fonction promise qui n'existe pas).
    ACCOUNT['guest'] = True
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_timeout(200)
    check('Compte invité' in page.inner_text('main') and 'SALT, PEPPER' in page.inner_text('main'), 'un compte invité voit le bandeau de récupération')
    ACCOUNT['guest'] = False
    # Thème sombre.
    page.emulate_media(color_scheme='dark'); page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(800); page.screenshot(path=str(OUT / 'board-dark.png'))
    # Mobile.
    page.set_viewport_size(dict(width=390, height=800)); page.wait_for_timeout(300); page.screenshot(path=str(OUT / 'board-mobile.png'))
    br.close()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print('RESULT', 'FAIL' if failures else 'OK')
sys.exit(1 if failures else 0)
