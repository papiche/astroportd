"""Station simulée pour les essais navigateur : API factice, fichiers statiques RÉELS de la coque (`crates/astro-ui/web/`).

Utilisée par `smoke_ui.py` (test de fumée) et `docs/roman/roman_photo.py` (captures des romans-photos)."""
import base64, io, json, math, random, sys, threading, time
from urllib.parse import unquote as _uq
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from PIL import Image, ImageDraw
from playwright.sync_api import sync_playwright

WEB = Path(__file__).resolve().parents[2] / 'crates' / 'astro-ui' / 'web'
random.seed(7)
MIME = {'.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.webmanifest': 'application/manifest+json'}

def qr_svg(text):
    """Vrai QR (bibliothèque qrcode), comme `astro_ui::qr::svg` : noir sur blanc, marge comprise."""
    import qrcode, qrcode.image.svg
    buf = io.BytesIO(); qrcode.make(text, image_factory=qrcode.image.svg.SvgPathFillImage, border=4).save(buf); return buf.getvalue().decode()


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

COOP = dict(exists=True, updated_at=1790000000, did='did:nostr:' + 'ab' * 32, unmanaged=['ZZ_LEGACY'], sections=[dict(title='Règle 3×1/3', fields=[dict(key=k, value=v, secret=False) for k, v in [('TREASURY_PERCENT', '33.33'), ('RND_PERCENT', '33.33'), ('ASSETS_PERCENT', '33.33'), ('CAPTAIN_BONUS_PERCENT', '1')]]), dict(title='OpenCollective', fields=[dict(key='OCSLUG', value='uplanet', secret=False), dict(key='OCAPIKEY', secret=True, set=True, readable=True)]),
    dict(id='ovh', title='DNSLink OVH', level='essential', fields=[dict(key=k, secret=True, set=True, readable=True) for k in ('OVH_APP_KEY', 'OVH_APP_SECRET', 'OVH_CONSUMER_KEY')] + [dict(key='OVH_ZONE', value='coop.example', secret=False)])])
ROUTE = dict(mesh=True, stations=[
    dict(peer='12D3KooWAmiLyon', name='Station de Marie', power_score=40, distance_km=3, services=[dict(name='ssh', local=None), dict(name='ollama', local=None)]),
    dict(peer='12D3KooWAmiNantes', name='Atelier Nantes', power_score=80, distance_km=520, services=[dict(name='ollama', local=None), dict(name='comfyui', local=None)]),
    dict(peer='12D3KooWAmiFerme', name='La Ferme', power_score=10, distance_km=40, services=[]),
    dict(peer='12D3KooWAmiLoin', name='Station éteinte', power_score=5, distance_km=None, services=dict(error='délai dépassé'))])
ADMIN = dict(face=dict(mode='off', label=None, locked=None, onnx_compiled=True, models='/m', library=None, pending=3, failed=0),
             kinds=dict(kinds=[dict(kind=1, label='Note publique', read='public', write='circle', project=True, default='read=public write=circle project=on', source='défaut'), dict(kind=7, label='Réaction (+N = Ẑen)', read='circle', write='circle', project=True, default='read=circle write=circle project=on', source='défaut')],
                        choices=dict(read=['public', 'addressed', 'friends', 'circle', 'deny'], write=['circle', 'open', 'deny']), fixed={'1059': 'x', '22242': 'y'}))
ADMIN.update(route=ROUTE, tunnels=None, tasks=None, assist=dict(services=['ssh'], friends=[], incoming=[], outgoing=[]))  # réglages facultatifs de m-options.js : absents, l'écran s'en passe sans erreur
# Tableau de bord par défaut : la forme de `GET /api/v1/dashboard` (src/ui_backend/station.rs), pour que Santé et Mes Ẑen
# s'affichent sans que chaque test le remplisse ; un test peut toujours le remplacer par STATE['dashboard'].
DASHBOARD = dict(backup_rs=None, station=dict(node='abcdef012345', peer_id='12D3KooWMaStation', domain='station.example', version='1.10.0', uptime_secs=86400 * 3, captain=True),
                 relay=dict(events=48210, last_event_at=1790000000, db_bytes=310_000_000), p2p=dict(enabled=True, peers=4, listen=2, score=12.5, tier='Standard'),
                 balance=dict(g1_cents=12000, zen_cents=120000, error=None, stale=False), energy=dict(weeks=4.2, max_weeks=8, weekly_rent_cents=100),
                 sovereignty=dict(encrypted_files=12, encrypted_bytes=2_200_000_000, last_manifest_at=1790000000, last_manifest_event='abcdef012345', last_manifest_rev='7'))
# Partages (m-services.js) : ce que cette station prête, les ports qui écoutent ici, une demande d'aide reçue.
TUNNELS = dict(origin=False, modes=['off', 'listed', 'friends', 'swarm'], max_shared=8, ssh_managed=True, friends=[dict(peer='12D3KooWAmiLyon', label='Marie'), dict(peer='12D3KooWAmiNantes', label='Atelier Nantes')],
               services=[dict(name='ssh', mode='off', default_mode='off', customized=False, grants=[], named=0, shared=False, target='127.0.0.1:22', up=True),
                         dict(name='ollama', mode='listed', default_mode='friends', customized=True, grants=[dict(peer='12D3KooWAmiLyon', label='Marie', until=1791000000, expired=False)], named=1, shared=False, target='127.0.0.1:11434', up=True),
                         dict(name='comfyui', mode='friends', default_mode='friends', customized=False, grants=[], named=0, shared=False, target='127.0.0.1:8188', up=False)],
               suggest=[dict(name='bureau-vnc', target='127.0.0.1:5900', label='Bureau à distance (VNC)')],
               listening=[dict(port=5900, target='127.0.0.1:5900', local_only=False, program='wayvnc', name='bureau-vnc', label='Bureau à distance (VNC)'),
                          dict(port=8096, target='127.0.0.1:8096', local_only=False, program='jellyfin', name='jellyfin', label='Films et musique (Jellyfin)'),
                          dict(port=7070, target='127.0.0.1:7070', local_only=True, program='navidrome', name='navidrome', label=None)])
ADMIN['tunnels'] = TUNNELS
# Capitaines : Marie est aussi dans l'annuaire (pas de doublon attendu) ; Paul n'y est pas (son service doit apparaître quand même).
ADMIN['captains'] = dict(captains=[dict(label='Marie', hex='dd' * 32, friend=True, i_follow=True, follows_me=True, peer='12D3KooWAmiLyon', services=[dict(name='ollama', local=None)]),
                                   dict(label='Paul', hex='ee' * 32, friend=True, i_follow=True, follows_me=True, peer='12D3KooWPaulHorsAnnuaire', services=[dict(name='jellyfin', local=None)]),
                                   dict(label='Inès', hex='ff' * 32, friend=False, i_follow=True, follows_me=False, peer=None, services=None)])
ADMIN['assist'] = dict(services=['ssh', 'bureau-vnc'], friends=[dict(hex='dd' * 32, label='Marie')], outgoing=[],
                       incoming=[dict(id='a1', other='dd' * 32, **{'from': 'Marie'}, service='ssh', hours=2, note='mon disque est plein', status='pending', station='12D3KooWAmiLyon')])
ADMIN['locks'] = dict(mode='village', plane=dict(origin=True, kubo=True, pool_peers=1, p2p=True), gates=dict(register=True, register_cli=True, crawl=True, n2=True, mutual_only=False, photos_auto=True, open_writes=[]), people=dict(hosted=4, n1=9, n1_mutual=5, n2=37, n2_known=37), faces=dict(auto_off_members=1), public=dict(files=12, unserved=0, sensitive=3, members=1))
# Essaim (m-mesh.js, m-swarm.js) : cette station, Marie (en ligne, Capitaine ami) et une station vue sans Capitaine connu.
ADMIN['mesh'] = dict(active=True, plane='ORIGIN', peer_id='12D3KooWMaStation', connected=['12D3KooWAmiLyon'], addrs=['/ip4/203.0.113.7/tcp/4101/p2p/12D3KooWMaStation'], relays=[], relay_server=False, bootstrap=[])
CONSTELLATION = dict(origin=dict(lat=43.6, lon=1.44), nodes=[
    dict(node_id='12D3KooWMaStation', self=True, power_score=30.7, tier='Standard', distance_km=0.0, status='local', services=['ai-index', 'ollama'], name='station.example', storage_free_gb=812, storage_total_gb=1000),
    dict(node_id='12D3KooWAmiLyon', self=False, power_score=56, tier='Brain', distance_km=337.1, status='connected', services=['ollama'], name='Station de Marie', url='https://marie.example', mode='village', uplanet='Ğ1 Libre', used=12, total=50, open=True, cpu_cores=16, ram_gb=64.0, gpu_gb=24, storage_free_gb=3200, storage_total_gb=4000, software='astroportd/2.0.0', at=1790000000),
    dict(node_id='12D3KooWVueSansCap', self=False, power_score=43, tier='Brain', distance_km=0.8, status='known', services=[], name='…SansCap'),
])
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
STATE = dict(faces=make_faces(), posts=[], deleted=[], chat=[])
# Historique des virements du membre (`GET /api/v1/me/zen/history`, src/constellation.rs::member_history) : centimes entiers.
_H = lambda i, d, c, **kw: {**dict(id=f'h{i}', at=1790000000 - i * 86400, dir=d, g1_cents=c, zen_cents=c * 10, other='g1Inconnu7Zs9q', name=None, coop=False, memo=None), **kw}
ZEN_HISTORY = [dict(indexer=True, next='WyJuZXh0Il0=', items=[_H(0, 'in', 150, name='Léa', memo='Pain du marché'), _H(1, 'out', 30, name='Marc'), _H(2, 'in', 1000, coop=True, kind='credit'), _H(3, 'out', 5)]),
               dict(indexer=True, next=None, items=[_H(4, 'in', 2500, name='Station de Marie', memo='Merci pour la réparation')])]
# Lecture des documents : vue du membre (état modifiable par le test) et réglages du Capitaine.
# Fiche plante (m-plant.js) : forme de `plant_fiche::view` / `edit` / `recognize` ; propositions tirées d'une vraie réponse de
# Pl@ntNet, rubriques tirées du vrai article de Wikipédia (fr) sur l'ail des ours.
PLANT = dict(available=True, fiche=None)
PLANT_PROPS = [dict(scientific='Allium ursinum', author='L.', common=['Ail des ours', 'Ail sauvage'], genus='Allium', family='Amaryllidaceae', score=87, gbif='2856681', powo='528475-1', danger=None),
               dict(scientific='Convallaria majalis', author='L.', common=['Muguet'], genus='Convallaria', family='Asparagaceae', score=6, gbif='2742212', powo='534427-1', danger="Muguet : toxique, feuilles confondues avec l'ail des ours.")]
PLANT_WIKI = dict(description="L'ail des ours est une plante herbacée vivace de la famille des Amaryllidacées.", edible='Ses feuilles se consomment crues dans les salades, se préparent sous forme de pesto et soupe.',
                  preparation='Alimentation humaine — On peut manger ses feuilles comme légume ou condiment.', medicinal="Thérapeutique — L'ail des ours est une plante médicinale très ancienne.",
                  dangers="Confusions possibles avec d'autres plantes — Avant floraison, l'ail des ours peut être confondu avec l'arum tacheté, le colchique d'automne, le muguet de mai.",
                  source=dict(title='Ail des ours', url='https://fr.wikipedia.org/wiki/Ail_des_ours', license='CC BY-SA 4.0'))
def plant_fiche(path):
    return PLANT['fiche'] or dict(v=1, sha='ab' * 32, photo=path, species=None, proposals=[], description='', edible='', preparation='', medicinal='', dangers='', notes='', source=None, updated_at=0)

READING = dict(state='off', blocked=None, stations=[], local=[], progress=dict(read=0, by_friend=0, failed=0, waiting=37, more=False), photos=None, tasks=[dict(id='ocr', ready=True)], paused=False, pack=None)
OFFERS = dict(stations=[dict(peer='12D3KooWMarie', name='La station de Marie', distance_km=12.4, power=31.0), dict(peer='12D3KooWAtelier', name='Atelier du Lez', distance_km=None, power=12.0)], silent=1)
ACCOUNTS = [dict(hex='bb' * 32, email='bob@example.org', role='multipass', status='cooperative_member', quota_bytes=10 ** 10, used_bytes=2 * 10 ** 9, unlimited=False, weekly_zen=1, files=420,
                 profile=dict(name='Bob Martin', picture='/ipfs/bafkreiboblogo', banner='/ipfs/bafkreibobban', website='https://bob.example.org/atelier/'),
                 last_activity=int(time.time()) - 3600, since=int(time.time()) - 400 * 86400, removable=False),
            dict(hex='cc' * 32, email='chloe@example.org', role='multipass', status='active_rental', quota_bytes=10 ** 10, used_bytes=10 ** 8, unlimited=False, weekly_zen=1, own_weekly_zen=1, files=12,
                 last_activity=int(time.time()) - 5 * 86400, since=int(time.time()) - 90 * 86400, removable=False, billet_issuer=True),
            dict(hex='dd' * 32, email='dormeur@example.org', role='multipass', status='active_rental', quota_bytes=10 ** 10, used_bytes=9 * 10 ** 9, unlimited=False, weekly_zen=1, files=3,
                 last_activity=int(time.time()) - 45 * 86400, since=int(time.time()) - 200 * 86400, removable=True, deletable=True, onboarding=dict(born_at=int(time.time()) - 200 * 86400, proved_at=1, primo='paid', ejectable=False)),
            dict(hex='ee' * 32, email='parti@example.org', role='multipass', status='active_rental', quota_bytes=0, used_bytes=3 * 10 ** 8, unlimited=False, weekly_zen=1, files=57,
                 last_activity=int(time.time()) - 80 * 86400, since=int(time.time()) - 300 * 86400, removable=False, deletable=True, removed_at=int(time.time()) - 20 * 86400)]
ADMIN['index'] = dict(enabled=True, default=True, engine=[], lend=None, members=[dict(hex='bb' * 32, email='bob@example.org', enabled=True, own=False, read=120, friend='La station de Marie'), dict(hex='cc' * 32, email='chloe@example.org', enabled=False, own=True, read=0, friend=None)])

# Agents (m-agents.js) : un agent déclaré, une demande en mode navigateur ; l'écran prêté est une page de connexion dessinée.
# IA du membre (m-ai.js) : l'Ollama de la station testé, OpenRouter testé (clé refusée).
AI_VIEW = dict(ollama_url='http://127.0.0.1:11434', provider='openrouter', has_key=True, model='llama3.2:3b', captain=True, can_own=True,
               rights=dict(mode='station', quota_tokens_week=200000, price_zen_week=2, own=False),
               station=dict(family='cloud', provider='MixRoute', model='deepseek-v3'), referrals=dict(mixroute='https://console.mixroute.ai/register?aff=COOP'),
               offers=[dict(url='http://127.0.0.1:11434', service='ollama', label='Ollama de cette station', friend=False), dict(url='http://127.0.0.1:24017', service='vllm', label="vLLM d'une station amie", friend=True)],
               providers=[dict(id='openrouter', label='OpenRouter'), dict(id='mixroute', label='MixRoute'), dict(id='mistral', label='Mistral'), dict(id='gemini', label='Gemini'), dict(id='anthropic', label='Anthropic')],
               tests=dict(ucloud=dict(family='ucloud', ok=True, model='llama3.2:3b', latency_ms=840, at=int(time.time()) - 120, models_total=2, caps=dict(chat=True, tools=None, vision=None, embeddings=True, images=None),
                                     models=[dict(id='llama3.2:3b', context=None, price_in=None, price_out=None), dict(id='bge-m3:latest')]),
                          cloud=dict(family='cloud', ok=False, error="clé d'API refusée : vérifiez-la, ou ses droits", at=int(time.time()) - 60, caps=dict(chat=None, tools=None, vision=None, embeddings=None, images=None))))
# IA des membres, vue du Capitaine (m-aiadmin.js).
STATION_AI = dict(mine=dict(family='cloud', provider='MixRoute', model='deepseek-v3'), test=dict(ok=True), share=True, shared_by_other=False,
                  default=dict(mode='station', quota_tokens_week=200000, price_zen_week=2), members={}, referrals=dict(mixroute='https://console.mixroute.ai/register?aff=COOP'),
                  providers=[dict(id='openrouter', label='OpenRouter'), dict(id='mixroute', label='MixRoute'), dict(id='mistral', label='Mistral'), dict(id='gemini', label='Gemini'), dict(id='anthropic', label='Anthropic')])
AGENTS = dict(agents=[dict(id='a1b2c3d4', name='OpenClaw', group='Agents', max_secs=3600, totp=False, created_at=1790000000)],
              requests=[dict(id='r1', agent='OpenClaw', client='Claude Code 2.1.3', remote=False, site='linkedin.com', reason='publier le résumé de la semaine', mode='navigateur', state='pending', asked_at=1790000000, until=None),
                        dict(id='r2', agent='OpenClaw', site='impots.gouv.fr', reason='relever ma facture', mode='accord', state='pending', asked_at=1790000000, until=None)],
              # Récemment : une demande rendue par l'agent aussitôt (cas réel d'OpenClaw qui « essaie » le flux), une restée sans réponse.
              recent=[dict(id='r3', agent='BORIS', site='example.org', reason='', mode='accord', state='returned', asked_at=int(time.time()) - 300, until=None),
                      dict(id='r4', agent='BORIS', site='framagit.org', reason='', mode='accord', state='expired', asked_at=int(time.time()) - 2400, until=None)])
# Naviguer ensemble : c1 tenue ici par Alice ; c2 tenue par la station d'un ami (Alice y est invitée).
COSURF = {'c1': dict(id='c1', owner='aa' * 32, owner_label='alice', mine=True, level=None, url='https://impots.gouv.fr/', until=4102444800, station='12D3KooWIci', guests=[dict(hex='ab' * 32, label='Léa Martin', level='drive')]),
          'c2': dict(id='c2', owner='cd' * 32, owner_label='Marc', mine=False, level='point', url='https://www.ameli.fr/', until=4102444800, station='12D3KooWAmi', guests=[dict(hex='aa' * 32, label='alice', level='point')])}

def login_page(n):
    im = Image.new('RGB', (800, 600), (245, 246, 248)); d = ImageDraw.Draw(im)
    d.rectangle((0, 0, 800, 56), fill=(10, 102, 194)); d.text((24, 20), 'linkedin.com', fill=(255, 255, 255))
    d.text((250, 140), 'Identifiez-vous', fill=(20, 20, 20)); d.rectangle((250, 180, 550, 220), outline=(120, 120, 120)); d.rectangle((250, 240, 550, 280), outline=(120, 120, 120))
    d.rectangle((250, 310, 550, 350), fill=(10, 102, 194)); d.text((370, 323), "S'identifier", fill=(255, 255, 255)); d.text((250, 380), f'image {n}', fill=(150, 150, 150))
    b = io.BytesIO(); im.save(b, 'JPEG', quality=60); return base64.b64encode(b.getvalue()).decode()

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

# Médiathèque : deux films, une série de deux épisodes, un mix de trois morceaux (fiches résumées comme les rend la station).
def _wav(seconds=2, rate=8000):
    import math, struct
    n = seconds * rate
    data = b''.join(struct.pack('<h', int(8000 * math.sin(2 * math.pi * 440 * i / rate))) for i in range(n))
    return b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVEfmt ' + struct.pack('<IHHIIHH', 16, 1, 1, rate, rate * 2, 2, 16) + b'data' + struct.pack('<I', len(data)) + data
WAV = _wav()
def _m(path, typ, title, **kw):
    base = dict(path=path, fiche=path.rsplit('.', 1)[0] + '.media.json', type=typ, title=title, genres=[], keywords=[], cast=[], directors=[], art={}, subtitles=[], level='private', duration_s=245, mime='audio/mpeg' if typ == 'music' else 'video/mp4')
    return {**base, **kw}
MEDIA = [
    _m('/Vidéos/Films/Le Voyage de Chihiro (2001).mp4', 'film', 'Le Voyage de Chihiro', year=2001, duration_s=7500, genres=['Animation', 'Fantastique'], directors=['Hayao Miyazaki'], cast=['Rumi Hiiragi', 'Miyu Irino'], keywords=['esprits'], overview="Chihiro, dix ans, découvre un monde peuplé d'esprits.", **{'art_': 1}),
    _m('/Vidéos/Autres/Atelier vélo (2024).mp4', 'video', 'Atelier vélo', year=2024, duration_s=600, level='circle'),
    _m('/Vidéos/Séries/Kaamelott/Saison 01/Kaamelott S01E01.mp4', 'episode', 'Heat', series=dict(name='Kaamelott', season=1, episode=1), duration_s=210),
    _m('/Vidéos/Séries/Kaamelott/Saison 01/Kaamelott S01E02.mp4', 'episode', 'Les Tartes aux myrtilles', series=dict(name='Kaamelott', season=1, episode=2), duration_s=212),
] + [_m(f'/Musique/DJ Test/Mon Mix/0{i} - {a} - {t}.mp3', 'music', t, music=dict(artist=a, album='Mon Mix', album_artist='DJ Test', track=i, mix_start_s=(i - 1) * 200), level='circle' if i == 1 else 'private')
     for i, (a, t) in enumerate([('Artiste A', 'Titre Un'), ('Artiste B', 'Titre Deux'), ('Artiste C', 'Titre Trois')], 1)]
for it in MEDIA:
    if it.pop('art_', None): it['art'] = dict(poster=it['path'].rsplit('.', 1)[0] + '.poster.jpg')
    if it['type'] == 'music': it['art'] = dict(cover=it['path'].rsplit('.', 1)[0] + '.cover.jpg')

# Chez mes amis : Léa et Paul ont le même film (deux sources), Léa partage aussi un morceau ; Paul a une station hors ligne.
FRIENDS = dict(items=[
    dict(event_id='e1' * 32, x='a1' * 32, type='film', title='Le Château dans le ciel (1986)', year=1986, duration_s=7440, genres=['Animation'], author='11' * 32, author_label='Léa',
         seeders=[dict(hex='11' * 32, label='Léa'), dict(hex='22' * 32, label='Paul')], copied=None, asked_at=None, error=None),
    dict(event_id='e2' * 32, x='a2' * 32, type='music', title='Artiste Z — Matin', duration_s=200, genres=[], music=dict(artist='Artiste Z', album='Aube', album_artist=None, track=1),
         author='11' * 32, author_label='Léa', seeders=[dict(hex='11' * 32, label='Léa')], copied=None, asked_at=None, error=None)],
    friends=[dict(hex='11' * 32, label='Léa', at=1, error=None), dict(hex='22' * 32, label='Paul', at=1, error='tunnel : connexion fermée')])

# Île « Ğ1 en différé » (m-island.js, m-credit.js) : réseau Ğ1 coupé, île ouverte, une promesse envoyée et une reçue.
def island_state():
    return dict(open=True, island='aaaaaaaaaaaaaaaa-1790000000', opened_at=1790000000, credit=True, available='15.00', available_zen='150.00', frozen=False, promises=[
        dict(id='p1', sent=True, author='aa' * 32, dest='ab' * 32, amount='5.00', amount_zen='50.00', at=1790000300, state='accepted', reason=None, settled=None, node='aa' * 32, known_station=True),
        dict(id='p2', sent=False, author='ef' * 32, dest='aa' * 32, amount='2.00', amount_zen='20.00', at=1790000600, state='accepted', reason=None, settled=None, node='ff' * 32, known_station=False)])
ISLAND_STATION = dict(open=True, island='aaaaaaaaaaaaaaaa-1790000000', opened_at=1790000000, snapshot_at=1789990000, snapshot_members=12, down_since=1789996000, auto_after_secs=1800,
                      promises=dict(accepted=7, refused=1), settlements=dict(), neighbors=[dict(island='ffffffffffffffff-1790000100', node='ff' * 32, members=4, at=1790000100, known_station=True, promises=3)], at=1790001000)

# « Se rencontrer » (m-pay.js) : je suis 'aa'*32 ; Léa et Marc sont déjà des proches ; Zoé est dans la constellation.
MEET_NAMES = {'ab' * 32: 'Léa', 'cd' * 32: 'Marc', '12' * 32: 'Zoé', 'ef' * 32: 'efefefef', '34' * 32: 'Hugo'}
MEET = dict(mine={'ab' * 32, 'cd' * 32}, back={'ab' * 32, 'cd' * 32}, met={}, asks=[], ranks={})
def meet_level(x): return MEET['ranks'].get(x, 3 if x in MEET['back'] else 1) if x in MEET['mine'] else None
def meet_status(x):
    m = MEET['met'].get(x)
    return dict(hex=x, label=MEET_NAMES.get(x, x[:8]), i_follow=x in MEET['mine'], follows_me=x in MEET['back'], friends=x in MEET['mine'] and x in MEET['back'], met=m, met_back=None, level=meet_level(x))
def circles():  # Mes cercles (m-circles.js) : ceux que je suis, du plus proche au plus lointain
    people = [dict(hex=x, label=MEET_NAMES.get(x, x[:8]), level=meet_level(x), ranked=x in MEET['ranks'], friends=x in MEET['back'], met_verified=bool(MEET['met'].get(x, {}).get('verified'))) for x in MEET['mine']]
    return dict(levels=[], people=sorted(people, key=lambda p: (-p['level'], p['label'].lower())))


class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def bro_answer(self, body):
        # Photos : réponse locale qui a lu les données ; « facture » sans IA locale : refus, accord demandé ; accord : cloud.
        if 'photo' in body['text'].lower(): return dict(reply='J’ai trouvé 3 photos de Léa à Toulouse, en juillet 2025.', where='ucloud', via='Ollama de cette station', source='station', model='llama3.2:3b', private=True, tools=['chercher'], tools_offered=True, tokens=280)
        if body.get('allow_cloud'): return dict(reply='Votre facture d’octobre : 74,20 € d’électricité, à payer avant le 15.', where='cloud', via='MixRoute', source='station', model='deepseek-v4-flash', private=True, tools=['chercher'], tools_offered=True, tokens=900)
        if body.get('private'): return dict(refused='private', cloud='MixRoute', reply='Cette conversation contient vos données privées, et votre station n’a pas d’IA qui tourne chez vous : je ne les envoie pas sur Internet sans votre accord.')
        return dict(reply='Bonjour ! Je suis BRO.', where='cloud', via='MixRoute', source='station', model='deepseek-v4-flash', private=False, tools=[], tools_offered=False, tokens=40)
    def send(self, code, body, ctype='application/json'):
        if not isinstance(body, bytes): body = json.dumps(body).encode()
        self.send_response(code); self.send_header('Content-Type', ctype); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        p = self.path.split('?')[0]
        if p == '/': return self.send(200, (WEB / 'index.html').read_bytes(), 'text/html')
        if p == '/register': return self.send(200, (WEB / 'register.html').read_bytes(), 'text/html')
        if p == '/api/v1/register': return self.send(200, dict(open=True, constellation='ORIGIN', accounts=1, max_accounts=500))
        # Présentation de la constellation (look.js) : ORIGIN par défaut ; `?look=zen` dans STATE pour une UPlanet privée renommée.
        if p == '/api/v1/look': return self.send(200, STATE.get('look') or dict(origin=True, name='ORIGIN', currency='Ẑen', symbol='Ẑ', style='kraft'))
        if p == '/stations': return self.send(200, (WEB / 'stations.html').read_bytes(), 'text/html')
        if p == '/api/v1/stations': return self.send(200, dict(stations=[dict(station='ici', name='Station ici', self=True, open=True, used=1, total=500, at=1790000000, lat=43.6, lon=1.44), dict(station='tls', name='Toulouse', open=True, used=3, total=50, url='https://toulouse.invalid', join='https://toulouse.invalid/register', at=1790000000, lat=43.61, lon=1.45)]))
        if p == '/api/v1/locate':
            q = self.path.split('q=', 1)[1] if 'q=' in self.path else ''; here = f'http://127.0.0.1:{self.server.server_address[1]}'
            STATE.setdefault('locate', []).append(q)
            if q.startswith('alice'): return self.send(200, dict(where='here'))
            if q.startswith('bob'): return self.send(200, dict(where='elsewhere', url=here, station='bob-station.example'))
            if q.startswith('eve'): return self.send(200, dict(where='elsewhere', url='https://eteinte.invalid', station='eteinte.invalid'))
            return self.send(200, dict(where='unknown'))
        if p == '/restore': return self.send(200, (WEB / 'restore.html').read_bytes(), 'text/html')
        if p == '/claim': return self.send(200, (WEB / 'claim.html').read_bytes(), 'text/html')
        if p == '/scan': return self.send(200, (WEB / 'scan.html').read_bytes(), 'text/html')  # Ğ1Billets : lecteur partagé avec « Payer »
        if p.startswith('/i/'): return self.send(200, (WEB / 'invite.html').read_bytes(), 'text/html')
        if p == '/api/v1/invites': return self.send(200, INVITES)
        if p == '/api/v1/invite/' + 'b2' * 16: return self.send(410, dict(error='ce lien a expiré : demandez-en un nouveau', code='invite_expired'))
        if p == f'/api/v1/invite/{TOKEN}': return self.send(200, dict(**{'from': 'alice'}, label='Léa', count=4, photos=4, thumbs=[0, 1], expires_at=1791000000, email_hint='l**@example.org'))
        if p.startswith('/ipfs/bafkrei'):  # images publiques des profils (écran Membres) : logo carré, bandeau large
            ban = p.endswith('ban')
            im = Image.new('RGB', (240, 80) if ban else (64, 64), (40, 110, 200) if ban else (220, 120, 40)); b = io.BytesIO(); im.save(b, 'PNG'); return self.send(200, b.getvalue(), 'image/png')
        if p.startswith(f'/api/v1/invite/{TOKEN}/thumb/'):
            im = Image.new('RGB', (16, 12), (180, 90, 60)); b = io.BytesIO(); im.save(b, 'PNG'); return self.send(200, b.getvalue(), 'image/png')
        if p.startswith('/ui/'):
            f = WEB / p[4:]
            return self.send(200, f.read_bytes(), MIME.get(f.suffix, 'application/octet-stream')) if f.is_file() else self.send(404, b'x', 'text/plain')
        if p == '/api/v1/agents': return self.send(200, dict(here='12D3KooWIci', agents=AGENTS['agents']))
        if p == '/api/v1/cosurf/view':
            from urllib.parse import parse_qs
            q = parse_qs(self.path.split('?', 1)[1]); STATE.setdefault('cosurf_views', []).append(q)
            import time
            s = COSURF.get(q['id'][0])
            if s: s['until'] = int(time.time()) + 3600
            return self.send(200, s) if s else self.send(404, dict(error='navigation partagée introuvable ou terminée'))
        if p == '/api/v1/cosurf/screen':
            from urllib.parse import parse_qs
            sid = parse_qs(self.path.split('?', 1)[1])['id'][0]
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            import time
            try:
                for n in range(40):
                    self.wfile.write(f'event: frame\ndata: {json.dumps(dict(seq=n + 1, w=800, h=600, jpeg=login_page(n)))}\n\n'.encode())
                    if n == 1: self.wfile.write(f'event: pointer\ndata: {json.dumps(dict(who="Léa Martin" if sid == "c1" else "Marc", hex="ab" * 32, x=0.4, y=0.5, at=1))}\n\n'.encode())
                    if sid == 'c2' and n == STATE.get('give_hand_at', 10 ** 6): self.wfile.write(b'event: level\ndata: {"level": "drive"}\n\n')
                    self.wfile.flush(); time.sleep(0.4)
            except (BrokenPipeError, ConnectionResetError): pass
            return
        if p == '/api/v1/agents/requests': return self.send(200, dict(requests=AGENTS['requests'], recent=AGENTS['recent']))
        if p == '/api/v1/agents/screen':
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            import time
            try:
                for n in range(40):
                    self.wfile.write(f'event: frame\ndata: {json.dumps(dict(seq=n + 1, w=800, h=600, jpeg=login_page(n)))}\n\n'.encode()); self.wfile.flush()
                    STATE['frames'] = n + 1; time.sleep(0.5)
            except (BrokenPipeError, ConnectionResetError): pass
            return
        if p == '/api/v1/me':
            if not (self.headers.get('Authorization') or '').startswith(('Basic ', 'Nostr ')): self.send(401, dict(error='authentification requise'))
            return self.send(200, dict(hex='aa' * 32, email='alice@example.org', captain=True, ai=STATE.get('ai', True), plants=True))
        if p == '/api/v1/me/meet/code':
            proof = f'{int(__import__("time").time()) // 120}.' + 'ab' * 64
            url = f'http://127.0.0.1:{self.server.server_port}/#/rencontre?to={"aa" * 32}&m={proof}'
            return self.send(200, dict(url=url, qr_svg=qr_svg(url), hex='aa' * 32, expires_in=60))
        if p.startswith('/api/v1/me/meet/'): return self.send(200, meet_status(p.rsplit('/', 1)[1]))
        if p == '/api/v1/me/circles': return self.send(200, circles())
        if p == '/api/v1/me/wot': return self.send(200, STATE.get('wot') or dict(member=False, root=False, certs=3, needed=5, reached_pct=100, mutual=1, certifiers=['Léa', 'Marc', 'Ana'], web=dict(members=42, referees=30, roots=4)))
        if p == '/api/v1/me/meet':
            from urllib.parse import urlparse, parse_qs
            q = parse_qs(urlparse(self.path).query).get('q', [''])[0].lower()
            out = dict(requests=[dict(hex=x, label=MEET_NAMES[x], at=1) for x in MEET['asks'] if x not in MEET['mine']])
            if q: out['people'] = [dict(meet_status(x), here=x != '12' * 32) for x, n in MEET_NAMES.items() if q in n.lower()]
            return self.send(200, out)
        if p.startswith('/api/v1/me/zen/receive'):  # « Recevoir » (m-pay.js) : un vrai QR, que smoke_pay.py relit
            from urllib.parse import urlparse, parse_qs
            a = parse_qs(urlparse(self.path).query).get('amount', [None])[0]
            url = f'http://127.0.0.1:{self.server.server_port}/#/payer?to={"aa" * 32}' + (f'&a={a}' if a else '')
            return self.send(200, dict(url=url, qr_svg=qr_svg(url), hex='aa' * 32, amount_zen=int(a) if a else None))
        if p == '/api/v1/me/ai': return self.send(200, dict(AI_VIEW, declared=STATE.get('ai', True)))
        if p == '/api/v1/station/ai': return self.send(200, STATION_AI)
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/account/site': return self.send(200, dict(name='alice', files=0, has_index=True, visits=0, host_url=None, path_url='/site/alice/'))
        if p == '/api/v1/presence': return self.send(200, dict(reachable=True, friends=[]))
        if p == '/api/v1/faces': return self.send(200, dict(enabled=False, unknown=0, suggestions=[]))
        if p == '/api/v1/account': return self.send(200, ACCOUNT)
        if p == '/api/v1/account/images': return self.send(200, dict(images=[], used=0, quota=1000000))
        if p == '/api/v1/contacts': return self.send(200, CONTACTS)
        if p == '/api/v1/me/links': return self.send(200, dict(plane='origin', siblings=['zen1'], keys=[dict(plane='origin', hex='aa' * 32, via='fiche'), dict(plane='zen1', hex='ee' * 32, via='station')]))
        if p == '/api/v1/island': return self.send(200, STATE.setdefault('island', island_state()))
        if p == '/api/v1/me/zen/history':  # Mes Ẑen › historique (m-history.js) : deux pages, la seconde par le curseur
            return self.send(200, ZEN_HISTORY[1] if 'after=' in self.path else ZEN_HISTORY[0])
        if p == '/api/v1/station/island': return self.send(200, ISLAND_STATION)
        if p == '/api/v1/friends': return self.send(200, dict(friends=[dict(hex='ab' * 32, label='Léa', hosted=False), dict(hex='cd' * 32, label='Marc', hosted=True)]))
        if p == '/api/v1/chat':
            c = STATE['chat']; unread = sum(1 for m in c if not m['mine'] and not STATE.get('chat_read'))
            return self.send(200, dict(unread=unread, plane='origin', conversations=[dict(peer='ab' * 32, label='Léa', last=c[-1]['text'], at=c[-1]['at'], mine=c[-1]['mine'], unread=unread, plane='origin', channels=[dict(plane='origin', peer='ab' * 32, unread=unread), dict(plane='zen1', peer='cd' * 32, unread=0)])] if c else []))
        if p == '/api/v1/chat/messages': return self.send(200, dict(peer='ab' * 32, messages=STATE['chat'], more=False))
        if p == '/api/v1/chat/settings': return self.send(200, dict(receipts=STATE.get('receipts', True)))
        if p == '/health': return self.send(200, dict(status='ok'))
        if p == '/api/v1/me/footprint': return self.send(200, dict(disk=dict(used_bytes=2_400_000_000, files=5321, quota_bytes=10_000_000_000, unlimited=False, tier='Basic', by_kind=dict(ucloud=dict(photo=1_500_000_000, video=600_000_000, music=120_000_000, doc=40_000_000, other=20_000_000), public=dict(photo=90_000_000, doc=30_000_000)), spread=STATE.get('spread')),
                                                                    opex=dict(total_28d_cents=400, weekly_rent_cents=100, rent_28d_cents=400), capex=dict(assets=[], zencard=False, net_cents=0, value_cents=0, amortized_cents=0)))  # Mes Ẑen (m-money.js)
        if p == '/api/v1/station/constellation': return self.send(200, CONSTELLATION)
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/faces/folders': return self.send(200, dict(folders=[dict(dir='/Images/2026/10', faces=12, todo=9), dict(dir='/Images/2026/09', faces=4, todo=0)]))
        if p == '/api/v1/faces/board':
            import time; time.sleep(STATE.get('delay', 0))
            return self.send(200, dict(enabled=True, dav='/dav', faces=STATE['faces'], people=people(), unknown=sum(1 for f in STATE['faces'] if not f['named']), groups_skipped=False, truncated=False))
        if p.startswith('/dav/') and _uq(p[4:]) in STATE.setdefault('dav', {}): return self.send(200, STATE['dav'][_uq(p[4:])], 'text/plain')
        if p.startswith('/dav/'): return self.send(200, base64.b64decode('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'), 'image/gif')
        if p == '/api/v1/station/planes': return self.send(200, dict(configured=True, multi_process=True, front=True, planes=[dict(name='origin', origin=True, http='127.0.0.1:8090', db='/x/uplanet.db', db_exists=True, running=True, domain='station.exemple.org'), dict(name='zen1', origin=False, http='127.0.0.1:8091', db='/x/zen1/uplanet.db', db_exists=False, running=False, domain=None)], front_tls=False, https=None, restart_needed=True, pending=['zen1'], path='/x/planes.json', service=dict(unit=STATE.get('unit', 'serve'), path='/h/.config/systemd/user/astroportd.service', command='/usr/bin/astroportd planes', mismatch=STATE.get('unit', 'serve') == 'serve', restart=True)))
        if p.startswith('/api/v1/book'):
            STATE.setdefault('gets', []).append(self.path)
            lea = dict(name='Léa Martin', given='Léa', family='Martin', org='Atelier', title='', phones=[dict(kind='mobile', value='+33 6 12 34 56 78')], emails=[dict(kind='home', value='lea@example.org')], addresses=[], birthday='', note='', urls=[], groups=['Famille'], about='', picture='', nip05='', lud16='')
            if p == '/api/v1/book': return self.send(200, dict(total=3, letters=['L', 'P'], groups=[dict(name='Famille', count=1)], contacts=[
                dict(path='/Contacts/Moi.vcf', name='Alice', org='', title='', emails=['alice@example.org'], phones=[], groups=[], letter='A', me=True, pubkey='aa' * 32, via=None, warning=None, faces=0, picture=''),
                dict(path='/Contacts/Lea.vcf', name='Léa Martin', org='Atelier', title='', emails=['lea@example.org'], phones=lea['phones'], groups=['Famille'], letter='L', me=False, pubkey='ab' * 32, via='vcard', warning=None, faces=12, picture=''),
                dict(path='/Contacts/Paul.vcf', name='Paul Durand', org='', title='', emails=[], phones=[], groups=[], letter='P', me=False, pubkey=None, via=None, warning=None, faces=0, picture='')]))
            if p == '/api/v1/book/me': return self.send(200, dict(path='/Contacts/Moi.vcf', me=True, pubkey='aa' * 32, fields=dict(lea, name='Alice', given='Alice', family='', org='', phones=[], emails=[dict(kind='home', value='alice@example.org')], groups=[], about='Capitaine'), faces=0, hosted_here='alice@example.org', profile=None, diff=None, drift=False))
            if p == '/api/v1/book/card': return self.send(200, dict(path='/Contacts/Lea.vcf', me=False, pubkey='ab' * 32, via='vcard', fields=lea, faces=12, hosted_here=None, plane='origin', keys=[dict(plane='origin', hex='ab' * 32, via='station'), dict(plane='zen1', hex='cd' * 32, via='lien')], profile=dict(name='Léa M.'), drift=True, diff=[
                dict(key='name', label='Nom', mine='Léa Martin', theirs='Léa M.', same=False, can_take=True), dict(key='about', label='Présentation', mine='', theirs='Boulangère', same=False, can_take=True), dict(key='nip05', label='NIP-05', mine='', theirs='', same=True, can_take=False)]))
            if p == '/api/v1/book/photo': return self.send(200, dict(uri=None))
            if p == '/api/v1/book/photo-choices': return self.send(200, dict(faces=[7], photos=[dict(id=7, path='/Photos/lea.jpg', bbox=[0.3, 0.2, 0.4, 0.5])], total=12))  # forme de address_book::photo_choices
            if p == '/api/v1/book/duplicates': return self.send(200, dict(groups=[dict(reasons=['même téléphone'], cards=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=[], phones=['+33 6 12']), dict(path='/Contacts/Lea2.vcf', name='L. Martin', emails=[], phones=['06 12'])])]))
        if p == '/api/v1/ucloud/memories': return self.send(200, STATE.get('memories') or dict(memories=[]))
        if p == '/api/v1/ucloud/trips': return self.send(200, STATE.get('trips') or dict(trips=[], home=None))  # smoke_souvenirs.py
        if p == '/api/v1/ucloud/person': return self.send(200, STATE.get('person') or dict(name='?', photos=0, items=[], years=[], trips=[]))
        if p == '/api/v1/ucloud/list' and 'Favoris' in self.path:
            return self.send(200, dict(dav='/dav', entries=[dict(name=k.rsplit('/', 1)[1], path=k, type='file', mime='application/internet-shortcut', size=len(v), mtime=1795000000) for k, v in STATE.setdefault('dav', {}).items() if k.startswith('/Favoris/')]))
        if p == '/api/v1/ucloud/list': return self.send(200, dict(dav='/dav', entries=[dict(name='Documents', path='/Documents', type='dir', mime=None, size=None, mtime=1790000000), dict(name='notes.txt', path='/notes.txt', type='file', mime='text/plain', size=120, mtime=1790000000)]))
        if p == '/api/v1/me/blog/friends': return self.send(200, dict(posts=[dict(author='cd' * 32, name='Léa', picture=None, site='/site/lea/', d='potager', title='Mon potager en novembre', summary='Ce qui pousse encore quand il fait froid.', image=None, tags=['jardin'], published_at=1795100000, reserved=False, minutes=3, html='<p>Les poireaux tiennent bon.</p><h2>Les choux</h2><p>Ils adorent le froid.</p>'), dict(author='ef' * 32, name='Paul', picture=None, site='/site/paul/', d='fete', title='La fête de samedi', summary='Rien que pour la famille.', image=None, tags=[], published_at=1795000000, reserved=True, minutes=1, html='<p>Rendez-vous à 19 h.</p>')]))
        if p == '/api/v1/me/blog': return self.send(200, dict(groups=[dict(id='ab' * 32, name='Famille', members=4)], theme='aurore', site='/site/alice/', hex='aa' * 32, posts=[dict(draft=False, d='levain', title='Mon levain a dix ans', summary='Farine, eau, patience.', image=None, tags=['pain'], published_at=1795000000, updated_at=1795000000, scheduled=False, content='Texte'), dict(draft=True, d='coop', title='Pourquoi une coopérative ?', summary='', image=None, tags=[], published_at=1795100000, updated_at=1795100000, scheduled=False, content='Brouillon')]))
        if p == '/api/v1/me/reading': return self.send(200, READING)
        if p == '/api/v1/me/reading/offers': return self.send(200, OFFERS)
        if p == '/api/v1/station/accounts': return self.send(200, dict(accounts=ACCOUNTS))
        if p == '/api/v1/station/hosting': return self.send(200, dict(default_quota_bytes=10 ** 10, default_weekly_zen=1))
        if p == '/api/v1/ucloud/search':  # un scan et un PDF retrouvés par le texte lu dedans (OCR), une photo par son nom
            from urllib.parse import parse_qs, urlsplit
            STATE.setdefault('searches', []).append(parse_qs(urlsplit(self.path).query))
            hit = lambda path, mime, x: dict(path=path, mime=mime, size=10, mtime=1790000000, taken_at=None, t=1790000000, thumb=None, lat=None, lon=None, **({'found': 'text', 'excerpt': x} if x else {}))
            items = [hit('/Scans/scan001.jpg', 'image/jpeg', '…FACTURE N° 2026-117 Électricité du hameau des Écluses…'), hit('/Documents/contrat-énergie.pdf', 'application/pdf', '…fourniture d’électricité, échéance trimestrielle…'), hit('/Photos/electricien.jpg', 'image/jpeg', None), dict(hit('/Photos/anniv.jpg', 'image/jpeg', None), found='seen'), dict(hit('/Vidéos/plage.mp4', 'video/mp4', None), found='seen', at_ms=83000), dict(hit('/Vidéos/Films/Le Voyage de Chihiro (2001).mkv', 'video/x-matroska', '…un voyage au pays des esprits…'), found='fiche', media=True)]
            if 'zanzibar' in self.path: return self.send(200, dict(items=[], next=None, understood=[], maybe=[items[3], items[1]]))
            if 'escale' in self.path: return self.send(200, dict(items=[], next=None, understood=[], maybe=[]))
            return self.send(200, dict(items=items, next=None, understood=[dict(text='lea', **{'as': ['person']}, people=['Léa Martin'], month=None)], best=[items[1], items[4], items[0]]))
        if p == '/api/v1/me/find':  # hors fichiers : un message, une note d'amie, un article de Léa, une fiche du carnet (smoke_search.py)
            if 'zanzibar' in self.path or 'rien' in self.path: return self.send(200, dict(messages=[], notes=[], contacts=[]))
            if 'escale' in self.path: return self.send(200, dict(messages=[dict(peer='bb' * 32, id='m1', at=1790000000, mine=False, group=False, name='Léa Martin', excerpt='On fait escale à Zanzibar en mars ?')], notes=[], contacts=[]))
            return self.send(200, dict(messages=[dict(peer='bb' * 32, id='m1', at=1790000000, mine=False, group=False, name='Léa Martin', excerpt='La facture d’électricité est arrivée'), dict(peer='dd' * 32, id='m2', at=1789000000, mine=True, group=True, name='', excerpt='je paie la facture demain')],
                notes=[dict(id='ee' * 32, author='bb' * 32, mine=False, name='Léa Martin', article=True, d='energie', title='Baisser sa facture', at=1788000000, excerpt='…notre facture a baissé de moitié…'), dict(id='ff' * 32, author='bb' * 32, mine=False, name='Léa Martin', article=False, d=None, title=None, at=1787000000, excerpt='Qui connaît un électricien ?')],
                contacts=[dict(path='/Contacts/enercoop.vcf', name='Enercoop', org='SCIC')]))
        if p == '/api/v1/ucloud/plants': return self.send(200, dict(available=True, items=[dict(photo='/Photos/ail.jpg', name='Ail des ours', scientific='Allium ursinum', edible=True, medicinal=True, toxic=False, updated_at=1), dict(photo='/Photos/muguet.jpg', name='Muguet', scientific='Convallaria majalis', edible=False, medicinal=False, toxic=True, updated_at=2)]))
        if p == '/api/v1/ucloud/plant': return self.send(200, dict(available=PLANT['available'], exists=PLANT['fiche'] is not None, fiche=plant_fiche(_uq(self.path.split('path=', 1)[1]))))
        if p == '/api/v1/ucloud/text': return self.send(200, dict(text='FACTURE N° 2026-117\nÉlectricité du hameau des Écluses' if 'scan001' in self.path else None))
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
        if p == '/api/v1/station/update': return self.send(200, STATE.get('update') or dict(current='1.7.0', latest='v1.7.0', newer=False, error=None, models=[], plugins=[], running=False, log=[], can_run=True, command='curl … | bash'))  # smoke_update.py
        if p == '/api/v1/dashboard': return self.send(200, STATE.get('dashboard') or DASHBOARD)  # accueil d'un membre (m-home.js) ; Détails techniques : smoke_backup_rs.py
        if p == '/api/v1/ucloud/media/safety': return self.send(200, STATE.get('safety') or dict(files={}))  # copies des fichiers partagés : smoke_backup_rs.py
        if p == '/api/v1/backup': return self.send(200, STATE.get('backup') or dict(mine=None, friends=[], hosting=[], share=None))  # gardiens : smoke_guardians.py
        if p == '/api/v1/faces/pending-shares': return self.send(200, dict(pending=[], auto_share=True))
        if p == '/api/v1/activity': return self.send(200, b': ok\n\n', 'text/event-stream')
        if p == '/api/v1/ucloud/media': return self.send(200, dict(items=MEDIA, pending=0))
        if p == '/api/v1/ucloud/media/friends':
            # Une copie demandée arrive à la relecture suivante : rangée comme chez l'ami, elle entre dans la médiathèque.
            for it in FRIENDS['items']:
                if it['asked_at'] and not it['copied']:
                    it['copied'] = '/Vidéos/Films/Le Château dans le ciel (1986).mp4'
                    MEDIA.append(_m(it['copied'], 'film', 'Le Château dans le ciel', year=1986, duration_s=7440))
            return self.send(200, FRIENDS)
        if p == '/api/v1/ucloud/media/fiche':
            from urllib.parse import unquote
            it = next(i for i in MEDIA if i['fiche'] == unquote(self.path.split('path=', 1)[1]))
            f = dict(type=it['type'], title=it['title'], original_title=None, year=it.get('year'), overview=it.get('overview'), genres=it['genres'], keywords=it['keywords'])
            if it['type'] == 'music': f['music'] = dict(artist=it['music']['artist'], album=it['music']['album'], album_artist='DJ Test', track=it['music']['track'], composer=None)
            else: f.update(cast=it['cast'], directors=it['directors'], **({'series': dict(it['series'])} if it['type'] == 'episode' else {}))
            return self.send(200, f)
        if p.startswith('/play/'): return self.send(200, WAV, 'audio/wav')
        if p == '/api/v1/ucloud/media/trace':
            return self.send(200, dict(shared=True, **{'from': None}, copies=[dict(label='Léa', at=1790000000)], reshared=['Léa']))
        self.send(404, dict(error='simulé : ' + p))
    def _dav_write(self):
        from urllib.parse import unquote as uq
        n = int(self.headers.get('Content-Length') or 0); body = self.rfile.read(n) if n else b''
        path = uq(self.path.split('?')[0][4:])
        if self.command == 'PUT': STATE.setdefault('dav', {})[path] = body
        if self.command == 'DELETE': STATE.setdefault('dav', {}).pop(path, None)
        STATE.setdefault('dav_ops', []).append((self.command, path))
        return self.send(201 if self.command != 'DELETE' else 204, b'', 'text/plain')
    do_PUT = do_MKCOL = _dav_write

    def do_DELETE(self):  # WebDAV dans /dav ; ailleurs, les DELETE de l'API (liens, invitations)
        if self.path.startswith('/dav/'): return self._dav_write()
        STATE['deleted'].append(self.path); self.send(200, dict(revoked=True))

    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); body = json.loads(self.rfile.read(n) or b'{}'); p = self.path
        STATE['posts'].append((p, body))
        if p == '/api/v1/me/circles':
            if body['hex'] not in MEET['mine']: return self.send(400, dict(error="ajoutez d'abord cette personne à vos proches"))
            if body.get('level') is None: MEET['ranks'].pop(body['hex'], None)
            else: MEET['ranks'][body['hex']] = body['level']
            return self.send(200, circles())
        if p.startswith('/api/v1/me/meet/'):
            x = p.rsplit('/', 1)[1]
            if (body.get('proof') or '').startswith('1.'): return self.send(400, dict(error="ce code a expiré : demandez à la personne de le remontrer"))
            MEET['mine'].add(x); MEET['met'].setdefault(x, dict(how='qr' if body.get('proof') else 'named', verified=bool(body.get('proof'))))
            return self.send(200, meet_status(x))
        if p == '/api/v1/me/zen/preview' and STATE.get('island', {}).get('open'):
            g1 = body['amount_zen'] * 10
            return self.send(200, dict(kind='member', payer='member', to=body['to'], email='lea@example.org', address='g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq', amount_zen_cents=body['amount_zen'] * 100, g1_cents=g1,
                                       activation_g1_cents=0, payer_g1_cents=1500, payer_after_g1_cents=1500 - g1, payer_after_zen_cents=(1500 - g1) * 10, split=[], max_zen=10000, island=dict(island='aaaaaaaaaaaaaaaa-1790000000', available_g1_cents=1500)))
        if p == '/api/v1/me/zen/send' and STATE.get('island', {}).get('open'): return self.send(200, dict(ok=True, island=True, promise=dict(id='p3'), amount_zen_cents=body['amount_zen'] * 100))
        if p == '/api/v1/me/zen/preview':  # hors île : le nom vient de la station du payeur, jamais du QR
            g1 = body['amount_zen'] * 10
            return self.send(200, dict(kind='member', payer='member', to=body['to'], email='lea@example.org' if body['to'] == 'ab' * 32 else body['to'][:8], address='g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq', amount_zen_cents=body['amount_zen'] * 100, g1_cents=g1,
                                       activation_g1_cents=0, payer_g1_cents=1500, payer_after_g1_cents=1500 - g1, payer_after_zen_cents=(1500 - g1) * 10, split=[], max_zen=10000))
        if p == '/api/v1/me/zen/send': return self.send(200, dict(ok=True, amount_zen_cents=body['amount_zen'] * 100))
        if p.startswith('/api/v1/station/island/'):
            op = p.rsplit('/', 1)[1]
            if op == 'open': ISLAND_STATION['open'] = True
            if op == 'close': ISLAND_STATION['open'] = False
            if op == 'auto': ISLAND_STATION['auto_after_secs'] = None if body.get('minutes') is None else body['minutes'] * 60
            return self.send(200, dict(ok=True))
        if p == '/api/v1/faces/thumbs': return self.send(200, dict(thumbs={str(i): thumb(i) for i in body['ids']}, failed=[]))
        if p == '/api/v1/station/planes': return self.send(200, dict(name=body['name'], http='127.0.0.1:8092', p2p='/ip4/0.0.0.0/tcp/4102', swarm_key_path='/x/zen2/swarm.key', created_key='/key/swarm/psk/1.0.0/\\n/base16/\\n' + 'ab' * 32))
        if p == '/api/v1/me/ai':
            STATE['ai'] = bool(body.get('ollama_url') or body.get('provider')); return self.send(200, dict(ollama_url=body.get('ollama_url'), provider=body.get('provider'), has_key=False, model=None, declared=STATE['ai']))
        if p == '/api/v1/bro/stream':
            # Même scénario que /bro/say, en flux : la route, puis la réponse par morceaux (80 ms), puis « done ».
            done = self.bro_answer(body)
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            ev = lambda d: (self.wfile.write(f'data: {json.dumps(d)}\n\n'.encode()), self.wfile.flush())
            if done.get('where'):
                ev(dict(type='route', via=done['via'], where=done['where'], model=done['model'])); ev(dict(type='turn'))
                if done.get('tools'): ev(dict(type='tool', name='chercher')); __import__('time').sleep(.2); ev(dict(type='turn'))
                for w in done['reply'].split(' '): ev(dict(type='delta', text=w + ' ')); __import__('time').sleep(.08)
            ev(dict(done, type='done')); return
        if p == '/api/v1/bro/say': return self.send(200, self.bro_answer(body))
        if p == '/api/v1/me/ai/test': return self.send(200, AI_VIEW['tests'].get(body['family'], {}))
        if p == '/api/v1/station/ai':
            if 'member' in body: STATION_AI['members'][body['member']['hex']] = body['member']['rights']
            if 'default' in body: STATION_AI['default'] = body['default']
            if 'share' in body: STATION_AI['share'] = body['share']
            return self.send(200, STATION_AI)
        if p == '/api/v1/book/delete-many': return self.send(200, dict(deleted=len(body['paths']), failed=[]))
        if p == '/api/v1/ucloud/plant':
            f = plant_fiche(body['path']); best = PLANT_PROPS[0]
            if not f['species']: f.update({k: v for k, v in PLANT_WIKI.items() if not f.get(k)}, species=dict(scientific=best['scientific'], common=best['common'], family=best['family'], gbif=best['gbif']))
            f['proposals'] = PLANT_PROPS; PLANT['fiche'] = f
            return self.send(200, dict(fiche=f, warning=None))
        if p == '/api/v1/ucloud/plant/fiche':
            f = plant_fiche(body['path']); e = body['fiche']
            f.update({k: e.get(k, '') for k in ('description', 'edible', 'preparation', 'medicinal', 'dangers', 'notes')}, species=e.get('species'))
            if e.get('wiki'): f.update({k: v for k, v in PLANT_WIKI.items() if not f.get(k)})
            PLANT['fiche'] = f
            return self.send(200, dict(fiche=f, warning=None))
        if p == '/api/v1/ucloud/rotate': return self.send(200, dict(rotated=len(body['paths']), failed=[]))
        if p == '/api/v1/ucloud/media/share':
            for it in MEDIA:
                if it['fiche'] in body['paths']: it['level'] = 'circle' if body['on'] else 'private'
            return self.send(200, dict(shared=body['on'], events=len(body['paths']), list=(body.get('list') or {}).get('kind')))
        if p == '/api/v1/ucloud/media/friends/request':
            it = next(i for i in FRIENDS['items'] if i['event_id'] == body['event_id'])
            if it['copied']: return self.send(200, dict(state='copied', path=it['copied']))
            it['asked_at'] = 1790000000; return self.send(200, dict(state='asked', at=it['asked_at']))
        if p == '/api/v1/ucloud/media/play': return self.send(200, dict(urls={x: f'/play/{i}' for i, x in enumerate(body['paths']) if not x.endswith('.jpg')}, expires_at=4102444800))
        if p == '/api/v1/ucloud/media/edit':
            it = next(i for i in MEDIA if i['fiche'] == body['path'])
            it.update(title=body['set']['title'], keywords=body['set'].get('keywords', it['keywords']), genres=body['set'].get('genres', it['genres']))
            return self.send(200, dict(item=it))
        if p.startswith('/api/v1/book'): return self.send(200, dict(path='/Contacts/Lea.vcf', taken=body.get('keys'), imported=2, duplicates=1, rejected=[], merged=True, removed=1, changed=1, fields=body.get('fields') or body))
        if p == '/api/v1/me/links': return self.send(200, dict(linked=[dict(plane='zen1', key='ee' * 32)], skipped=[]))
        if p == '/api/v1/chat/send':
            q = next((x for x in STATE['chat'] if x['id'] == body.get('reply')), None)
            m = dict(id=str(len(STATE['chat'])), mine=True, at=1790000000 + len(STATE['chat']) * 60, text=body['text'], state='ok', seen=False, reply=body.get('reply'), quote=q and dict(text=q['text'], mine=q['mine']), files=body.get('files', []))
            STATE['chat'].append(m); return self.send(200, m)
        if p == '/api/v1/chat/read': STATE['chat_read'] = True; return self.send(200, dict(read=1))
        if p == '/api/v1/chat/react':
            m = next(x for x in STATE['chat'] if x['id'] == body['id']); m['reactions'] = [dict(emoji=body['emoji'], n=1, mine=True, by=['aa' * 32])] if body['emoji'] else []
            return self.send(200, dict(id=body['id'], emoji=body['emoji']))
        if p == '/api/v1/chat/edit':
            m = next(x for x in STATE['chat'] if x['id'] == body['id']); m.update(text=body['text'], edited=True); return self.send(200, m)
        if p == '/api/v1/chat/typing': return self.send(200, dict(sent=1))
        if p == '/api/v1/chat/prefs': STATE.setdefault('prefs', []).append(body); return self.send(200, dict(peer=body['peer']))
        if p == '/api/v1/chat/settings': STATE['receipts'] = body['receipts']; return self.send(200, dict(receipts=body['receipts']))
        if p == '/api/v1/chat/delete': STATE['chat'] = [m for m in STATE['chat'] if m['id'] != body['id']]; return self.send(200, dict(deleted=True))
        if p == '/api/v1/me/zen/recipients': return self.send(200, dict(recipients=[dict(to='cash', label='cash'), dict(to='ab' * 32, label='lea@example.org')], max_zen=10000))
        if p == '/api/v1/faces/delete-photos': return self.send(200, dict(deleted=len({f['path'] for f in STATE['faces'] if f['id'] in body['ids']}), missing=0, bytes=0))
        if p == '/api/v1/faces/assign':
            for f in STATE['faces']:
                if f['id'] in body['ids']: f.update(name=body['name'], pubkey=body.get('pubkey'), named=True, group_id=None, group_size=None, suggest=None)
            return self.send(200, dict(assigned=len(body['ids']), name=body['name']))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/devices': return self.send(200, dict(id='d2', label=body['label'], root=body['root'], mode=body['mode'], token='dv_' + 'ab' * 16, note='Ce jeton ne sera plus jamais affiché : copiez-le maintenant.'))
        if p == '/api/v1/cosurf':
            STATE.setdefault('cosurf_created', []).append(body)
            return self.send(200, dict(COSURF['c1'], url='https://' + body['url'].split('://')[-1].split('/')[0] + '/'))
        if p == '/api/v1/cosurf/level':
            for g in COSURF[body['id']]['guests']:
                if g['hex'] == body['guest']: g['level'] = body['level']
            return self.send(200, COSURF[body['id']])
        if p == '/api/v1/cosurf/input': STATE.setdefault('cosurf_inputs', []).append(body); return self.send(200, dict(ok=True))
        if p == '/api/v1/cosurf/stop': return self.send(200, dict(id=body['id'], ended=True))
        if p.startswith('/api/v1/agents'):
            reqs = {r['id']: r for r in AGENTS['requests']}
            if p == '/api/v1/agents':
                a = dict(id='e5f6a7b8', name=body['name'], group=body['group'], max_secs=body['max_secs'], totp=False, created_at=1790000000); AGENTS['agents'].append(a)
                return self.send(200, dict(id=a['id'], name=a['name'], token='ag1_' + 'aa' * 8 + '_' + 'cd' * 32, note='montré une fois'))
            if p == '/api/v1/agents/revoke': AGENTS['agents'] = [a for a in AGENTS['agents'] if a['id'] != body['id']]; return self.send(200, dict(revoked=body['id']))
            if p == '/api/v1/agents/input': STATE.setdefault('inputs', []).append(body); return self.send(200, dict(ok=True))
            r = reqs.get(body.get('id'))
            if r is None: return self.send(404, dict(error='agent ou demande introuvable'))
            import time
            if p == '/api/v1/agents/decide':
                if not body['approve']: AGENTS['requests'].remove(r); return self.send(200, dict(r, state='denied'))
                r.update(state='login' if r['mode'] == 'navigateur' else 'granted', until=int(time.time()) + (600 if r['mode'] == 'navigateur' else body.get('secs', 900)), secs=body.get('secs', 900))
                return self.send(200, r)
            if p == '/api/v1/agents/ready': r.update(state='granted', until=int(time.time()) + r.get('secs', 900)); return self.send(200, r)
            if p == '/api/v1/agents/stop': AGENTS['requests'].remove(r); return self.send(200, dict(r, state='revoked'))
        if p == '/api/v1/devices/revoke': return self.send(200, dict(revoked=body['id']))
        if p == '/api/v1/organize/preview': return self.send(200, dict(total=7, placed=1, protected=4, unknown=1, capped=False, by_kind=[dict(kind='images', folder='Images', files=2, bytes=3000, sample=[dict(**{'from': '/Imports/A/p.jpg', 'to': '/Images/p.jpg'})]), dict(kind='documents', folder='Documents', files=5, bytes=900, sample=[])]))
        if p == '/api/v1/organize/undo': return self.send(200, dict(restored=7))
        if p.startswith('/api/v1/organize') or p.startswith('/api/v1/pull'): return self.send(200, dict(ok=True))
        if p == '/api/v1/station/admin/dnslink': return self.send(200, dict(ok=False, error='This credential is not valid') if body['values'].get('OVH_APP_SECRET') == 'faux' else dict(ok=True, zone='coop.example'))
        if p == '/api/v1/station/restart': return self.send(200, dict(restarting=True))
        if p == '/api/v1/station/update': STATE.setdefault('update_posts', []).append(1); STATE['update'] = {**STATE.get('update', {}), 'running': True, 'log': ['▶ Téléchargement et vérification', '  ✔ empreinte SHA-256 vérifiée']}; return self.send(200, dict(started=True, version='v1.8.0'))
        if p == '/api/v1/station/planes/service': STATE['unit'] = 'planes'; return self.send(200, dict(state='switched', unit='/h/u', command='x', backup='/h/u.bak-1', carried=[], carried_to=None, to_carry=[], unreadable=False))
        if p == '/api/v1/station/admin/captains':  # suivre / ne plus suivre (Captains::act) : la vue revient
            hx = body.get('follow') or body.get('unfollow'); c = next(x for x in ADMIN['captains']['captains'] if x['hex'] == hx)
            c['i_follow'] = 'follow' in body; c['friend'] = c['i_follow'] and c['follows_me']; STATE['followed'] = body; return self.send(200, ADMIN['captains'])
        if p == '/api/v1/station/admin/locks': ADMIN['locks']['mode'] = body['mode']; return self.send(200, ADMIN['locks'])
        if p == '/api/v1/station/admin/tunnels':  # comme TunnelPolicy::set : share, unshare, mode, grant, revoke, reset
            sv = {s['name']: s for s in TUNNELS['services']}
            if 'share' in body:
                s = body['share']
                if s['name'] in sv: return self.send(400, dict(error=f"« {s['name']} » existe déjà"))
                TUNNELS['services'].append(dict(name=s['name'], mode=s.get('mode', 'listed'), default_mode='listed', customized=False, grants=[], named=0, shared=True, target=s['target'], up=True))
                TUNNELS['listening'] = [x for x in TUNNELS['listening'] if x['target'] != s['target']]
            elif 'unshare' in body: TUNNELS['services'] = [s for s in TUNNELS['services'] if s['name'] != body['unshare']]
            else:
                s = sv[body['service']]
                if 'mode' in body: s.update(mode=body['mode'], customized=True)
                if 'grant' in body: s['grants'].append(dict(peer=body['grant']['peer'], label=body['grant'].get('label') or '', until=None if not body['grant'].get('hours') else 1791000000, expired=False))
                if 'revoke' in body: s['grants'] = [g for g in s['grants'] if g['peer'] != body['revoke']]
                if body.get('reset'): s.update(mode=s['default_mode'], customized=False, grants=[])
            return self.send(200, TUNNELS)
        if p == '/api/v1/station/admin/assist':
            r = next((x for x in ADMIN['assist']['incoming'] if x['id'] == body.get('id')), None)
            if body.get('action') == 'respond' and r: r['status'] = 'accepted' if body['accept'] else 'declined'
            return self.send(200, dict(ok=True))
        if p == '/api/v1/station/admin/route':  # services ouverts par la constellation : ouvrir (station précise ou la plus puissante), fermer
            rows = ADMIN['route']['stations']
            if 'connect' in body:
                c = body['connect']; peer = c.get('peer') or max((r for r in rows if isinstance(r['services'], list) and any(x['name'] == c['service'] for x in r['services'])), key=lambda r: r['power_score'])['peer']
                if c['service'] == 'comfyui': return self.send(400, dict(error='la station amie refuse ce flux'))
                # Comme la station : port habituel (Ollama, ComfyUI), sinon port stable de la plage 24000–24999.
                STATE['last_connect'] = c; port = c.get('port') or {'ollama': 11434, 'comfyui': 8188}.get(c['service']) or 24000 + len(c['service'])
                for r in rows:
                    for x in (r['services'] if r['peer'] == peer and isinstance(r['services'], list) else []):
                        if x['name'] == c['service']: x['local'] = dict(addr=f'127.0.0.1:{port}', until=1791000000)
                return self.send(200, dict(peer=peer, service=c['service'], addr=f'127.0.0.1:{port}', until=1791000000, route=None))
            for r in rows:
                for x in (r['services'] if isinstance(r['services'], list) else []):
                    if x['name'] == body['disconnect']['service'] and r['peer'] == body['disconnect'].get('peer', r['peer']): x['local'] = None
            return self.send(200, dict(closed=1))
        if p.startswith('/api/v1/station/accounts/') and p.endswith('/remove'):
            hx = p.split('/')[5]
            a = next((x for x in ACCOUNTS if x['hex'] == hx), None)
            if not a or not a.get('removable'): return self.send(400, dict(error='compte actif : il ne se retire qu’après 30 jours sans activité'))
            ACCOUNTS.remove(a); return self.send(200, dict(removed=a['email'], cashback_g1_cents=0))
        if p.startswith('/api/v1/station/accounts/') and p.endswith('/delete'):
            a = next((x for x in ACCOUNTS if x['hex'] == p.split('/')[5]), None)
            if not a or not a.get('deletable'): return self.send(400, dict(error='compte actif : il ne se supprime qu’après 30 jours sans activité'))
            if body.get('confirm', '').strip().lower() != a['email']: return self.send(400, dict(error=f"pour supprimer sans retour, retapez l'adresse du compte ({a['email']})"))
            ACCOUNTS.remove(a); return self.send(200, dict(deleted=a['email'], files=a['files'], bytes=a['used_bytes'], events=3, blobs_freed=a['files'], farewell_signed=2, farewell_announced=2, cashback_g1_cents=0))
        if p.startswith('/api/v1/station/accounts/') and p.endswith('/hosting'):
            a = next(x for x in ACCOUNTS if x['hex'] == p.split('/')[5])
            a.update(own_quota_bytes=body.get('quota_bytes'), own_weekly_zen=body.get('weekly_zen'), weekly_zen=1 if body.get('weekly_zen') is None else body['weekly_zen'])
            if body.get('quota_bytes'): a['quota_bytes'] = body['quota_bytes']
            if 'billet_issuer' in body: a['billet_issuer'] = body['billet_issuer']
            return self.send(200, dict(hex=a['hex'], quota_bytes=body.get('quota_bytes'), weekly_zen=body.get('weekly_zen')))
        if p == '/api/v1/station/admin/index':
            ix = ADMIN['index']
            for k in ('enabled', 'default'):
                if k in body: ix[k] = body[k]
            if 'member' in body:
                m = next(x for x in ix['members'] if x['hex'] == body['member']['hex'])
                m.update(enabled=ix['default'] if body['member']['enabled'] is None else body['member']['enabled'], own=body['member']['enabled'] is not None)
            return self.send(200, ix)
        if p == '/api/v1/me/reading':
            if 'pause' in body: READING['paused'] = body['pause']; return self.send(200, READING)
            if body.get('erase'): READING.update(paused=True, pack=None, progress=dict(READING['progress'], read=0), photos=READING['photos'] and dict(READING['photos'], seen=0)); return self.send(200, READING)
            if body.get('revoke') is True: READING['stations'] = []
            elif isinstance(body.get('revoke'), str): READING['stations'] = [s for s in READING['stations'] if s['peer'] != body['revoke']]
            else: READING['stations'] += [dict(peer=p, label=next(s['name'] for s in OFFERS['stations'] if s['peer'] == p), tasks=body['accept']['tasks'], since=1790000000, read=3) for p in body['accept']['peers']]
            READING.update(state='friend' if READING['stations'] else 'off', progress=dict(read=3 * len(READING['stations']), by_friend=3, failed=0, waiting=34, more=False))
            return self.send(200, READING)
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/register': return self.send(200, dict(email='lea-x7k2p9@uplanet.local', token='JT', pass_='123456', salt='S', pepper='P', nsec='nsec1x', npub='npub1x', hex='cd' * 32, g1pub='G', nostrns='/ipns/k', constellation='ORIGIN', lang='fr', guest=True, card=CARD, card_svg=QR, **{'pass': '123456'}))
        if p == '/api/v1/restore/ask': return self.send(200, STATE.get('rescue') or dict(status='none'))  # secours par les gardiens : rien à attendre, sauf scénario (smoke_guardians.py)
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
        if p == '/api/v1/me/blog/preview':
            md = body.get('markdown', '')
            return self.send(200, dict(html=''.join(f'<h2>{l[3:]}</h2>' if l.startswith('## ') else f'<p>{l}</p>' for l in md.split('\n') if l.strip() and not l.startswith('# ')), minutes=max(1, len(md.split()) // 220 + 1)))
        if p == '/api/v1/me/blog/publish': return self.send(200, dict(event=dict(id='ee' * 32, kind=body.get('kind'))))
        if p == '/api/v1/me/blog/settings': return self.send(200, dict(theme=body.get('theme')))
        if p == '/api/v1/me/blog/image': return self.send(200, dict(path='/site/alice/images/x.jpg', rel='images/x.jpg'))
        if p == '/api/v1/ucloud/have':
            import hashlib
            owned = {hashlib.sha256(v).hexdigest(): k for k, v in STATE.setdefault('dav', {}).items()}
            return self.send(200, dict(have=[dict(sha=x, path=owned[x]) for x in body.get('shas', []) if x in owned]))
        if p == '/api/v1/feedback': return self.send(200, dict(ok=True, stored='git', issue_url='https://github.com/papiche/astroportd/issues/1', issue_number=1))
        if p == '/api/v1/ucloud/import-meta': STATE.setdefault('meta', []).extend(body['items']); return self.send(200, dict(updated=len(body['items'])))
        if p == '/api/v1/account/token/show': return self.send(200, dict(login='alice@example.org', token='jeton'))
        if p == '/api/v1/account/token': return self.send(200, dict(login='alice@example.org', token='NOUVEAU-JETON', generation=1))
        if p == '/api/v1/account/keys': return self.send(200, dict(nsec='nsec1secret', npub='npub1test', dunikey='sec: x', dunikey_error=None))
        if p == '/api/v1/account/profile': return self.send(200, ACCOUNT)
        if p == '/api/v1/faces/forget':
            STATE['faces'] = [f for f in STATE['faces'] if f['id'] not in body['ids']]; return self.send(200, dict(forgotten=len(body['ids'])))
        self.send(404, dict(error='simulé : ' + p))


def start(port=0):
    """Démarre la station simulée (port libre par défaut) ; rend `(serveur, port)`."""
    srv = ThreadingHTTPServer(('127.0.0.1', port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, srv.server_address[1]


if __name__ == '__main__':
    # Atelier de l'interface : `~/.astro/bin/python tests/browser/mock_station.py [port]`, puis ouvrir l'adresse dans un
    # navigateur. Les fichiers de `crates/astro-ui/web/` sont relus à chaque requête : on modifie, on recharge, sans compiler.
    srv, port = start(int(sys.argv[1]) if len(sys.argv) > 1 else 8765)
    print(f'station simulée : http://127.0.0.1:{port}/#/members  (Ctrl+C pour arrêter)')
    try: threading.Event().wait()
    except KeyboardInterrupt: srv.shutdown()
