"""Test de fumée navigateur de la table de tri et de la réplication : API simulée, fichiers statiques RÉELS de la coque.

Chromium (Playwright) pilote l'application : sélection par groupe, rangement, glisser-déposer vers une pochette, lasso,
« pas à reconnaître », écran de réplication, thèmes clair/sombre, mobile. Aucune erreur de console n'est tolérée.

    python3 tests/browser/smoke_ui.py [dossier-des-captures]

Hors de la CI (Playwright et Chromium requis) : à lancer avant de publier une modification de `crates/astro-ui/web/`.
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')

srv, port = start()
errors, failures = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

with sync_playwright() as pw:
    br = pw.chromium.launch(); ctx = br.new_context(viewport=dict(width=1280, height=820), service_workers='block'); page = ctx.new_page()
    page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
    ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    page.goto(f'http://127.0.0.1:{port}/#/people/tri'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(900)
    check(page.inner_text('.pe-count').startswith(f"{sum(1 for f in STATE['faces'] if not f['named'])} à ranger"), 'le compteur « à ranger » est affiché : ' + page.inner_text('.pe-count'))
    check(page.locator('.pe-pochette[data-drop="person"]').count() == 3, 'trois pochettes de personnes')
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
    page.select_option('.pe-tray select', page.evaluate("[...document.querySelectorAll('.pe-tray select option')].find(o => o.textContent.startsWith('Paul ('))?.value")); page.wait_for_timeout(700)  # les options sont « Nom (photos) », valeur = rang
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
    lea = page.locator('.pe-pochette[data-drop="person"]:has-text("Léa")').bounding_box()
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
    # Réplication : l'ancienne adresse mène à Moi › Sauvegarde, où les copies chez les autres stations sont repliées.
    page.goto(f'http://127.0.0.1:{port}/#/replication'); page.wait_for_selector('#bk-copies'); page.wait_for_timeout(200)
    check(page.evaluate('location.hash') == '#/backup', 'l\'ancienne adresse de la réplication mène à Sauvegarde')
    check('Suis-je à l’abri ?' in page.inner_text('main') and '90 % copiées' in page.inner_text('main'), 'Sauvegarde commence par un bilan, dont les copies de vos publications')
    page.click('#bk-copies summary'); page.wait_for_selector('.rp-ring'); page.wait_for_timeout(300)
    page.click('summary:has-text("Détails techniques")')  # les voies réseau sont repliées pour le membre
    txt = page.inner_text('main')
    check('27 sur 30' in txt and 'Proche de proche' in txt and 'Maillage P2P' in txt and 'Injoignable' in txt and 'connexion fermée' in txt, 'la réplication montre où, par qui et par quelle voie')
    check('5 sur 8' in txt and 'poppy : 5 fichier' in txt, 'les fichiers fournis par d\'autres stations sont listés')
    page.screenshot(path=str(OUT / 'replication.png'), full_page=True)
    # Configuration coopérative (Capitaine) : secrets en écriture seule, seules les clés modifiées partent.
    page.goto(f'http://127.0.0.1:{port}/#/coop'); page.wait_for_selector('form fieldset'); page.wait_for_timeout(200)
    check(page.locator('main input.secret').count() == 4 and page.locator('main input.secret').first.get_attribute('placeholder').startswith('● renseigné') and page.locator('main input[type=password]').count() == 0, 'les secrets sont masqués et jamais affichés (sans type=password, que le navigateur voudrait remplir)')
    check('ZZ_LEGACY' in page.inner_text('main'), 'les clés hors écran sont signalées')
    page.fill('#cf-OCSLUG', 'autre'); page.fill('#cf-TREASURY_PERCENT', '40'); page.wait_for_timeout(100)
    check('additionne' in page.inner_text('.degraded'), 'la somme des parts différente de 100 % est signalée')
    check(page.locator('main form button:has-text("Publier")').is_disabled(), 'la publication est bloquée tant que les parts modifiées ne font pas 100 %')
    check(page.locator('main h2 .pill').count() == 1, 'l\'en-tête dit quelle constellation est modifiée')
    page.click('button:has-text("Tester la connexion OVH")'); page.wait_for_selector('text=Connexion OVH réussie')
    page.fill('#cf-OVH_APP_SECRET', 'faux'); page.click('button:has-text("Tester la connexion OVH")'); page.wait_for_selector('text=This credential is not valid')
    sent_t = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/dnslink']
    check(len(sent_t) == 2 and sent_t[0]['action'] == 'test' and 'OVH_APP_SECRET' not in sent_t[0]['values'] and sent_t[1]['values']['OVH_APP_SECRET'] == 'faux', 'coop : l\'essai OVH envoie la saisie en cours (rien si le champ est vide) et dit le refus')
    page.fill('#cf-OVH_APP_SECRET', '')
    page.fill('#cf-TREASURY_PERCENT', '33.33')
    page.click('button:has-text("Publier")'); page.wait_for_timeout(150)
    check('OCSLUG' not in ''.join(b_ for p_, b_ in [(x, str(y)) for x, y in STATE['posts']] if p_ == '/api/v1/station/coop') and 'toute la constellation' in page.inner_text('main'), 'le premier clic résume sans publier')
    page.click('button:has-text("Confirmer la publication")'); page.wait_for_timeout(600)
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
    # Écluses : le canal suit le mode montré ; l'aperçu ne change rien tant qu'on n'a pas appliqué.
    page.set_viewport_size(dict(width=1100, height=900)); page.goto(f'http://127.0.0.1:{port}/#/locks'); page.wait_for_selector('svg.lk-canal'); page.wait_for_timeout(400)
    canal = lambda: page.evaluate("[document.querySelector('.lk-canal').dataset.mode, document.querySelector('.lk-canal').dataset.reach, [...document.querySelectorAll('.lk-gate')].map(g => g.getAttribute('class').split(' ')[1])]")
    check(canal() == ['village', '5', ['lk-shut', 'lk-open', 'lk-open', 'lk-open', 'lk-filter']], 'Village : les bateaux vont jusqu\'au public, coffre fermé : ' + str(canal()))
    check('5 ami(s) qui suivent en retour' in page.inner_text('main') and 'Kubo branché' in page.inner_text('main'), 'les chiffres et l\'état de la station sont affichés')
    page.screenshot(path=str(OUT / 'locks-village.png'), full_page=True)
    page.click('button.lk-mode[data-mode=forteresse]'); page.wait_for_timeout(900)
    check(canal() == ['forteresse', '2', ['lk-shut', 'lk-filter', 'lk-shut', 'lk-filter', 'lk-filter']] and 'Aperçu' in page.inner_text('.lk-apply'), 'aperçu Forteresse : les portes bougent, rien n\'est envoyé : ' + str(canal()))
    check(not [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/locks'], 'l\'aperçu n\'appelle pas la station')
    page.screenshot(path=str(OUT / 'locks-forteresse-apercu.png'), full_page=True)
    page.click('button:has-text("Appliquer ce mode")'); page.wait_for_timeout(500)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/station/admin/locks']
    check(sent == [dict(mode='forteresse')] and page.locator('.lk-mode.lk-now[data-mode=forteresse]').count() == 1, 'appliquer envoie le mode choisi : ' + str(sent))
    page.emulate_media(color_scheme='dark'); page.click('button.lk-mode[data-mode=place]'); page.wait_for_timeout(900)
    page.screenshot(path=str(OUT / 'locks-place-sombre.png'), full_page=True)
    page.emulate_media(color_scheme='light')
    page.set_viewport_size(dict(width=400, height=900)); page.wait_for_timeout(400)
    check(page.evaluate("document.querySelector('main').scrollWidth") <= 400, 'Écluses à 400 px : aucun débordement horizontal de l\'écran')
    page.screenshot(path=str(OUT / 'locks-400.png'), full_page=True)
    page.set_viewport_size(dict(width=1100, height=800))
    # Connexion en deux temps : l'identifiant d'abord ; un membre d'une autre station y est mené sans jamais taper son PASS ici.
    cl = br.new_context(viewport=dict(width=1100, height=800), service_workers='block'); pl = cl.new_page()
    pl.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text and 'ERR_NAME_NOT_RESOLVED' not in m.text else None)  # station « éteinte » fictive : son nom ne se résout pas
    pl.goto(f'http://127.0.0.1:{port}/'); pl.wait_for_selector('#login[open]')
    check(pl.inner_text('#login-h') == f'127.0.0.1:{port}' and pl.locator('#login-2').is_hidden(), 'la connexion nomme la station et ne demande d\'abord que l\'identifiant')
    pl.fill('#login-f input[name=u]', 'bob@ailleurs.org'); pl.click('#login-ok'); pl.wait_for_selector('#login-x:not([hidden])')
    check('bob-station.example' in pl.inner_text('#login-x') and pl.locator('#login-2').is_hidden() and pl.get_attribute('#login-x a.btn', 'href').endswith('/?u=bob%40ailleurs.org'), 'compte ailleurs : on y mène le membre, le champ PASS reste caché')
    pl.screenshot(path=str(OUT / 'login-ailleurs.png'))
    pl.fill('#login-f input[name=u]', 'zoe@nulle-part.org'); pl.click('#login-ok'); pl.wait_for_timeout(300)
    check('Aucun MULTIPASS' in pl.inner_text('#login-x') and pl.locator('#login-2').is_hidden(), 'compte inconnu : réponse neutre et chemins utiles')
    pl.fill('#login-f input[name=u]', 'alice@example.org'); pl.click('#login-ok'); pl.wait_for_selector('#login-2:not([hidden])')
    check(pl.locator('#login-x').is_hidden() and pl.inner_text('#login-ok') == 'Se connecter', 'compte ici : le PASS est demandé')
    check(not [b for p_, b in STATE['posts'] if p_ == '/api/v1/account/unlock'], 'aucun PASS n\'est parti pendant ces recherches')
    pl.goto(f'http://127.0.0.1:{port}/?u=bob%40ailleurs.org'); pl.wait_for_selector('#login[open]')
    check(pl.input_value('#login-f input[name=u]') == 'bob@ailleurs.org', 'arrivé depuis une autre station : l\'email est déjà rempli')
    # Portail /stations : deux portes ; « déjà membre » retrouve sa station, et dit quand elle ne répond pas.
    pl.goto(f'http://127.0.0.1:{port}/stations#membre'); pl.wait_for_selector('#member:not([hidden])')
    check(pl.get_attribute('#door-member', 'aria-selected') == 'true' and pl.locator('#new').is_hidden(), '#membre ouvre la porte des membres')
    pl.fill('#who', 'bob@ailleurs.org'); pl.click('#find button[type=submit]'); pl.wait_for_selector('#trip.is-go')
    check('bob-station.example' in pl.inner_text('#trip') and pl.get_attribute('#trip-act a', 'href').endswith('/?u=bob%40ailleurs.org'), 'station retrouvée et joignable : un bouton y mène')
    pl.click('#trip-act a'); pl.wait_for_timeout(300); pl.goto(f'http://127.0.0.1:{port}/stations'); pl.wait_for_selector('#last:not([hidden])')
    check(pl.get_attribute('#door-member', 'aria-selected') == 'true', 'déjà venu : la porte des membres s\'ouvre avec la dernière station')
    pl.fill('#who', 'eve@ailleurs.org'); pl.click('#find button[type=submit]'); pl.wait_for_selector('#trip.is-off', timeout=8000)
    check('à l’abri' in pl.inner_text('#trip') and pl.locator('#trip-act a[href="/restore"]').count() == 1, 'station éteinte : message rassurant, réessayer ou rétablir')
    pl.set_viewport_size(dict(width=400, height=900)); pl.wait_for_timeout(200)
    check(pl.evaluate('document.scrollingElement.scrollWidth') <= 400, 'portail à 400 px : aucun débordement')
    pl.screenshot(path=str(OUT / 'portail-400.png'), full_page=True)
    pl.click('#door-new'); pl.wait_for_timeout(300)
    check(pl.locator('#member').is_hidden() and pl.locator('#list a.btn').first.inner_text() == 'Rejoindre', 'porte des nouveaux : rejoindre une station')
    pl.set_viewport_size(dict(width=1100, height=800)); pl.click('#door-member'); pl.wait_for_timeout(200)
    check(pl.locator('#list a.btn').first.inner_text() == 'Ouvrir', 'porte des membres : ouvrir sa station depuis la liste')
    pl.screenshot(path=str(OUT / 'portail-membre.png'), full_page=True)
    cl.close()
    # Moi : une seule rangée de sous-onglets (Compte · Mes Ẑen · Mon blog · Appareils · Sauvegarde), pas d'onglets dans les onglets.
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_timeout(200)
    check(page.locator('nav.segs a').all_inner_texts() == ['Compte', 'Mes Ẑen', 'Mon blog', 'Appareils', 'Sauvegarde'] and page.locator('[role=tablist]').count() == 0, 'Moi : une seule rangée de sous-onglets')
    page.goto(f'http://127.0.0.1:{port}/#/account?tab=securite'); page.wait_for_selector('#bk-keys'); page.wait_for_timeout(100)
    check(page.evaluate('location.hash') == '#/backup', 'l\'ancien lien vers Ma sécurité mène à Sauvegarde')
    page.click('#bk-keys summary')  # les clés et l'identité sont repliées
    check('npub1test' in page.inner_text('main') and 'wss://s.example' in page.inner_text('main'), 'Mon compte montre l\'identité et les relais annoncés')
    check(page.locator('button:text-is("Exporter mes clés")').is_disabled(), 'l\'export des clés exige une case de confirmation')
    # L'API donne `/dav` : l'écran montre (et copie) l'adresse complète, et le bouton de remplacement n'effraie pas sans raison.
    page.goto(f'http://127.0.0.1:{port}/#/devices'); page.wait_for_selector('#dv-label', state='attached'); page.wait_for_timeout(100)
    main_text = page.inner_text('main')
    check(f'http://127.0.0.1:{port}/dav' in main_text, "l'adresse WebDAV est complète")
    # Le jeton principal se montre sans être remplacé (il est dérivé du secret de la station).
    page.click('button:text-is("👁 Afficher")'); page.wait_for_selector('input.ac-token')
    check(page.input_value('input.ac-token') == 'jeton', 'le jeton principal s\'affiche sans être remplacé')
    check(not page.locator('button:text-is("Remplacer le jeton principal")').is_visible(), 'le remplacement du jeton principal est replié')
    page.click('summary:text-is("Remplacer le jeton principal")'); page.click('button:text-is("Remplacer le jeton principal")'); page.click('button:text-is("Confirmer ?")')  # clic armé (plus de boîte bloquante) : le second clic confirme
    page.wait_for_selector('text=NOUVEAU-JETON'); page.wait_for_timeout(100)
    import base64 as b64
    page.wait_for_function("localStorage.getItem('basic') !== btoa('alice@example.org:jeton')", timeout=3000); got = b64.b64decode(page.evaluate("localStorage.getItem('basic')")).decode(); check(got == 'alice@example.org:NOUVEAU-JETON', 'la session suit le nouveau jeton : ' + got)
    page.goto(f'http://127.0.0.1:{port}/#/backup'); page.wait_for_selector('#bk-keys'); page.click('#bk-keys summary'); page.check('#ac-ok'); page.click('button:text-is("Exporter mes clés")'); page.wait_for_selector('textarea[aria-label=nsec]')
    check(page.input_value('textarea[aria-label=nsec]') == 'nsec1secret', 'les clés exportées s\'affichent après confirmation')
    check('[object' not in page.inner_text('main') and page.input_value('textarea[aria-label=dunikey]') == 'sec: x', 'la clé Ğ1 exportée est un champ, pas du texte « [object HTMLLabelElement] »')
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.fill('#ac-name', 'Alice B'); page.click('button:text-is("Enregistrer")'); page.wait_for_timeout(400)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/account/profile']
    check(sent and sent[-1] == dict(name='Alice B'), 'seul le champ modifié du profil est envoyé : ' + str(sent[-1:]))
    # Multilingue : l'anglais traduit la coque et les écrans, sans rechargement de dictionnaire côté serveur.
    ctx2 = br.new_context(viewport=dict(width=1280, height=820), service_workers='block'); p2 = ctx2.new_page()
    ctx2.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton')); localStorage.setItem('lang', 'en')")
    p2.goto(f'http://127.0.0.1:{port}/#/account'); p2.wait_for_selector('#ac-name'); p2.wait_for_timeout(400)
    nav = p2.inner_text('nav')
    check('Home' in nav and 'Me' in nav and 'Accueil' not in nav, 'la navigation est traduite en anglais : ' + nav.replace('\n', ' | '))
    check('Display name' in p2.inner_text('main'), 'l\'écran Mon compte est traduit')
    p2.goto(f'http://127.0.0.1:{port}/#/backup'); p2.wait_for_selector('#bk-keys'); p2.click('#bk-keys summary'); p2.wait_for_timeout(100)
    check('Export my keys' in p2.inner_text('main') and 'Am I safe?' in p2.inner_text('main'), 'Sauvegarde est traduite')
    p2.goto(f'http://127.0.0.1:{port}/#/devices'); p2.wait_for_selector('#ai-url'); p2.wait_for_timeout(200)
    check('Send only' in p2.inner_text('main') and 'Envoi seul' not in p2.inner_text('main'), 'Appareils est traduit')
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
    # Photo de la fiche : envoyer une image, prendre un visage reconnu, retirer ; sélection de fiches pour suppression groupée.
    page.click('.ab-head button:has-text("Photo…")'); page.wait_for_selector('.ab-picker .ab-face'); page.click('.ab-picker .ab-face >> nth=0'); page.wait_for_timeout(400)
    ph = [b for p_, b in STATE['posts'] if p_ == '/api/v1/book/photo']
    check(ph and str(ph[-1]['uri']).startswith('data:image/jpeg;base64,') and ph[-1]['path'] == '/Contacts/Lea.vcf', 'un visage reconnu devient la photo de la fiche')
    page.click('.ab-head button:has-text("Photo…")'); page.click('button:has-text("Retirer la photo")'); page.wait_for_timeout(400)
    check([b for p_, b in STATE['posts'] if p_ == '/api/v1/book/photo'][-1]['uri'] is None, 'la photo se retire')
    page.click('main .ab-more summary'); page.click('.ab-menu button:has-text("Sélectionner")'); page.locator('.ab-chk >> nth=1').check(); page.wait_for_timeout(200)
    check('1 sélectionnée(s)' in page.inner_text('main'), 'une fiche cochée : le compte est affiché')
    page.click('button:has-text("Tout sélectionner")'); page.click('button:has-text("Supprimer la sélection")'); page.wait_for_timeout(400)
    dm = [b for p_, b in STATE['posts'] if p_ == '/api/v1/book/delete-many']
    check(dm and len(dm[-1]['paths']) >= 1, 'la suppression groupée envoie les fiches cochées')
    page.click('button:has-text("Terminer la sélection") >> visible=true')
    page.reload(); page.wait_for_selector('.ab-list li button'); page.click('.ab-item:has-text("Léa Martin")'); page.wait_for_selector('.ab-card'); page.wait_for_timeout(300)
    check(page.input_value('#f-name') == 'Léa Martin' and page.locator('.ab-diff table tbody tr').count() == 2, 'la fiche affiche ses champs et la comparaison avec le profil Nostr')
    check(page.locator('.ck-acts a.btn:has-text("Écrire")').count() == 1 and page.get_attribute('.ck-acts a.btn:has-text("Écrire")', 'href') == '#/chat?peer=' + 'ab' * 32, "« Écrire » (en-tête de la fiche) ouvre la conversation avec la clé de l'UPlanet courante")
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
    page.click('main .ab-more summary'); page.click('.ab-menu button:has-text("Doublons")'); page.wait_for_selector('button:has-text("Garder celle-ci")'); page.wait_for_timeout(200)
    check('même téléphone' in page.inner_text('.ab-detail'), 'les doublons probables sont proposés avec leur raison')
    page.screenshot(path=str(OUT / 'contacts-book.png'))
    # Contacts : l'annuaire de synchronisation (onglet « Synchroniser et inviter »), la liaison manuelle et la création d'une fiche depuis une personne.
    def open_sync():
        page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_timeout(100); page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('main .ab-more summary'); page.click('main .ab-more summary'); page.click('.ab-menu button:has-text("Synchroniser et inviter")'); page.wait_for_selector('table')
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
        p3.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text else None); p3.on('pageerror', lambda e: errors.append(str(e)))
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
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('main .ab-more summary'); page.click('main .ab-more summary'); page.click('.ab-menu button:has-text("Synchroniser et inviter")'); page.wait_for_selector('table')
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
    p4.fill('input[name=u]', 'alice@example.org'); p4.click('#login-f button[value=ok]'); p4.wait_for_selector('#login-2', state='visible'); p4.fill('input[name=p]', '000000'); p4.click('#login-f button[value=ok]'); p4.wait_for_timeout(400)
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
    p5.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text else None); p5.on('pageerror', lambda e: errors.append(str(e)))
    p5.goto(f'http://127.0.0.1:{port}/register'); p5.wait_for_selector('#f:not([hidden])')
    p5.check('#guest'); p5.fill('#alias', 'léa'); p5.click('#go'); p5.wait_for_selector('#done:not([hidden])')
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/register']
    check(sent and sent[-1] == dict(guest=True, alias='léa', lang='fr'), 'l\'inscription sans email envoie un alias, pas d\'adresse : ' + str(sent[-1:]))
    check('lea-x7k2p9@uplanet.local' in p5.inner_text('#k-email') and '123456' in p5.inner_text('#k-pass'), 'l\'adresse générée et le PASS à 6 chiffres sont remis')
    p5.click('summary:has-text("Détails techniques")'); p5.wait_for_timeout(150)  # la carte technique est repliée : le membre voit d'abord l'essentiel
    check(p5.is_visible('#qr svg') and CARD in p5.inner_text('#card') and p5.is_visible('#print'), 'la clé de secours est remise en texte et en QR imprimable')
    # Rétablir son compte sur cette station depuis la carte : le champ est vidé dès la réponse, le nouveau PASS est remis.
    p6 = br.new_context(viewport=dict(width=390, height=800), locale='fr-FR', service_workers='block').new_page()
    p6.on('console', lambda m: errors.append(m.text) if m.type == 'error' and '401' not in m.text else None); p6.on('pageerror', lambda e: errors.append(str(e)))
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
    # Messages : liste, fil chiffré, envoi par Entrée, non lus remis à zéro à l'ouverture, ouverture depuis une fiche de contact.
    STATE['chat'] = [dict(id='a', mine=False, at=1790000000, text='Salut, tu viens samedi ?', state='ok')]; STATE['chat_read'] = False
    page.goto(f'http://127.0.0.1:{port}/#/chat'); page.wait_for_selector('.ab-list li button'); page.wait_for_timeout(300)
    check('Léa' in page.inner_text('.ab-list') and page.query_selector('.ab-list .ch-badge') is not None, 'la conversation apparaît avec son compteur de non lus')
    check(page.query_selector('nav a[data-r=people] .ch-badge') is not None, 'la pastille de non lus est dans le menu (Proches)')
    page.click('.ab-list li button'); page.wait_for_selector('.ch-msg'); page.wait_for_timeout(300)
    check('zen1' not in page.inner_text('.ab-list') and 'origin' not in page.inner_text('.ab-list'), 'la conversation ne montre ni constellation ni clé au membre')
    check('Salut, tu viens samedi ?' in page.inner_text('.ch-thread') and 'Chiffré de bout en bout' in page.inner_text('.ch-head'), 'le fil affiche le message reçu et la mention de chiffrement')
    check(page.query_selector('nav a[data-r=chat] .ch-badge') is None, "ouvrir la conversation retire la pastille")
    page.fill('.ch-form textarea', 'Oui, avec plaisir !'); page.press('.ch-form textarea', 'Enter'); page.wait_for_selector('.ch-msg.mine'); page.wait_for_timeout(300)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/chat/send']
    check(sent and sent[-1] == dict(peer='ab' * 32, text='Oui, avec plaisir !') and 'Oui, avec plaisir !' in page.inner_text('.ch-msg.mine'), 'Entrée envoie le message et il apparaît à droite')
    page.screenshot(path=str(OUT / 'chat.png'))
    page.goto(f'http://127.0.0.1:{port}/#/chat?peer=' + 'ab' * 32); page.wait_for_selector('.ch-msg'); page.wait_for_timeout(200)
    check('Oui, avec plaisir !' in page.inner_text('.ch-thread'), "le lien depuis une fiche ouvre directement la conversation")
    # Menu d'un message (clic droit) : citer et répondre ; la réponse part avec la citation ; coche « remis » ; Ẑen vers un proche d'ici.
    page.click('.ch-msg.theirs', button='right'); page.wait_for_selector('.ch-menu')
    check([b.inner_text() for b in page.query_selector_all('.ch-menu [role=menuitem]')] == ['↩️ Citer et répondre', '📋 Copier', '🗑️ Supprimer pour moi'] and len(page.query_selector_all('.ch-emojis button')) == 6, 'clic droit : six réactions, puis Répondre, Copier, Supprimer pour moi')
    page.click('.ch-menu button:has-text("Citer et répondre")'); page.wait_for_selector('.ch-replybar:not([hidden])')
    page.fill('.ch-form textarea', 'Je te le confirme'); page.press('.ch-form textarea', 'Enter'); page.wait_for_selector('.ch-quote'); page.wait_for_timeout(200)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/chat/send']
    check(sent[-1].get('reply') == 'a' and 'Salut, tu viens samedi ?' in page.inner_text('.ch-quote') and page.is_hidden('.ch-replybar'), 'la réponse part avec le message cité, le montre au-dessus, et le bandeau de réponse se ferme')
    check(page.query_selector('.ch-msg.mine .ch-tick') is not None, 'ses messages portent la coche « remis »')
    page.click('.ch-kebab'); page.wait_for_selector('.ch-head .ch-menu')
    items = [x.inner_text() for x in page.query_selector_all('.ch-head .ch-menu button')]
    check(items[0] == 'Ẑ Envoyer des Ẑen à Léa' and '📌 Épingler' in items and '🔕 Mettre en sourdine' in items and items[-1] == '🗑️ Effacer la conversation', 'menu ⋮ : payer, épingler, sourdine, confidentialité, effacer, en toutes lettres : ' + str(items))
    page.keyboard.press('Escape')
    # Réaction depuis le menu d'un message, puis modification de son propre message.
    page.click('.ch-msg.theirs', button='right'); page.wait_for_selector('.ch-emojis')
    page.click('.ch-emojis button:has-text("👍")'); page.wait_for_selector('.ch-react'); page.wait_for_timeout(200)
    check('👍' in page.inner_text('.ch-msg.theirs .ch-reacts'), 'une réaction apparaît sous le message')
    page.click('.ch-msg.mine >> nth=-1', button='right'); page.wait_for_selector('.ch-menu button:has-text("Modifier")')
    page.click('.ch-menu button:has-text("Modifier")'); page.wait_for_selector('.ch-replybar:not([hidden])')
    page.fill('.ch-form textarea', 'Je te le confirme, à samedi'); page.press('.ch-form textarea', 'Enter'); page.wait_for_timeout(300)
    check('à samedi' in page.inner_text('.ch-thread') and 'modifié' in page.inner_text('.ch-thread'), 'un message modifié porte la mention « modifié »')
    page.click('.ch-new'); page.wait_for_selector('.ch-pick li button'); page.wait_for_timeout(200)
    check('Léa' in page.inner_text('.ch-pick') and 'Vous vous suivez' in page.inner_text('.ch-pick'), '« Nouveau message » : les proches, avec ce qui les rend joignables')
    page.keyboard.press('Escape')
    page.set_viewport_size(dict(width=400, height=800)); page.wait_for_timeout(300)
    check(page.evaluate("document.querySelector('main').scrollWidth") <= 400, 'chat à 400 px : aucun débordement horizontal')
    page.screenshot(path=str(OUT / 'chat-400.png'))
    page.set_viewport_size(dict(width=1100, height=800))
    # Hors du chat, un message non lu : lien « 💬 1 » dans l'en-tête, vers la conversation.
    STATE['chat'].append(dict(id='z', mine=False, at=1790009000, text='Tu as vu ?', state='ok', seen=False)); STATE['chat_read'] = False
    page.goto(f'http://127.0.0.1:{port}/#/'); page.reload(); page.wait_for_selector('#chat-in'); page.wait_for_timeout(200)
    check('💬' in page.inner_text('#chat-in') and page.get_attribute('#chat-in', 'href').startswith('#/chat'), "un message non lu : lien d'accès au chat dans l'en-tête")
    page.click('#chat-in'); page.wait_for_selector('.ab-list li button'); page.wait_for_timeout(200)
    check(page.url.endswith('#/chat'), "le lien de l'en-tête ouvre le chat")
    # Mes UPlanet : les clés dans les autres plans et l'annonce publique, sur demande seulement.
    page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#announce-links', state='attached'); page.click('summary:has-text("Ce que je publie")'); page.wait_for_selector('#announce-links'); page.wait_for_timeout(200)
    check('Mes autres comptes' in page.inner_text('main') and 'zen1' not in page.inner_text('main'), 'Mon compte parle de « mes autres comptes », sans constellation ni clé')
    before = len([1 for p_, _ in STATE['posts'] if p_ == '/api/v1/me/links'])
    check(before == 0, "l'annonce ne part jamais d'elle-même : rien n'est publié à l'ouverture de l'écran")
    page.click('#announce-links'); page.wait_for_timeout(150)  # clic armé : le premier clic explique, rien n'est annoncé
    check(not [1 for p_, _ in STATE['posts'] if p_ == '/api/v1/me/links'] and 'publiquement' in page.inner_text('main'), "le premier clic dit que l'annonce est publique et n'annonce rien")
    page.click('#announce-links'); page.wait_for_timeout(300)
    check(len([1 for p_, _ in STATE['posts'] if p_ == '/api/v1/me/links']) == 1, "l'annonce part après la confirmation")
    # Thème sombre.
    page.emulate_media(color_scheme='dark'); page.goto(f'http://127.0.0.1:{port}/#/people/tri'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(800); page.screenshot(path=str(OUT / 'board-dark.png'))
    # Quitter un écran lent avant la fin de son chargement ne doit pas faire apparaître son erreur dans l'écran suivant.
    page.route('**/api/v1/faces/board', lambda route: (page.wait_for_timeout(2500), route.continue_()))
    page.goto(f'http://127.0.0.1:{port}/#/people'); page.wait_for_timeout(300)
    page.goto(f'http://127.0.0.1:{port}/#/contacts'); page.wait_for_selector('main >> .ab-search'); page.wait_for_timeout(4500)
    txt = page.inner_text('main')
    check('Nouvelle fiche' in txt and 'Cannot' not in txt and 'null' not in txt and 'Table de tri' not in txt, "l'écran quitté en cours de chargement n'écrit rien dans le suivant")
    page.unroute('**/api/v1/faces/board')
    # Appareils : un jeton WebDAV par téléphone, confiné à un dossier.
    page.goto(f'http://127.0.0.1:{port}/#/devices'); page.wait_for_selector('.ac-add summary'); page.click('.ac-add summary'); page.wait_for_selector('#dv-label'); page.wait_for_timeout(300)
    txt = page.inner_text('main')
    check('Téléphone de Léa' in txt and '/Photos/Léa' in txt and 'Envoi seul' in txt and 'Jamais utilisé' in txt, "l'appareil est listé avec son dossier, ses droits et son dernier usage")
    page.fill('#dv-label', 'Tablette'); page.select_option('#dv-mode', 'readwrite'); page.click('button:text-is("Ajouter cet appareil")'); page.wait_for_selector('text=ajouté'); page.wait_for_timeout(300)
    sent = [b for p_, b in STATE['posts'] if p_ == '/api/v1/devices']
    check(len(sent) == 1 and sent[0] == dict(label='Tablette', root='/Photos/Tablette', mode='readwrite'), "le dossier par défaut est /Photos/<nom> et les droits choisis partent")
    check(page.input_value('input[aria-label=Jeton]') == 'dv_' + 'ab' * 16 and 'ne sera plus jamais affiché' in page.inner_text('main'), 'le jeton est montré une fois, avec le serveur et le dossier')
    page.click('button:text-is("Révoquer")'); page.click('button:text-is("Confirmer ?")'); page.wait_for_timeout(400)  # clic armé
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
    # Constellations : liste, redémarrage nécessaire, ajout d'une constellation neuve (la clé créée est montrée pour être copiée).
    page.goto(f'http://127.0.0.1:{port}/#/planes'); page.wait_for_selector('input[aria-label="Nom de la constellation"]'); page.wait_for_timeout(500)
    check(page.locator('table tbody tr').count() == 2 and 'zen1' in page.inner_text('main'), 'les constellations de la station sont listées')
    check(page.locator('#plane option').count() == 2 and page.input_value('#plane') == 'origin', "le sélecteur de plan de l'en-tête propose ORIGIN et zen1")
    check('Redémarrez' in page.inner_text('main'), 'le redémarrage nécessaire est dit')
    page.fill('input[aria-label="Nom de la constellation"]', 'zen2'); page.click('form button:has-text("Ajouter")'); page.wait_for_selector('textarea[readonly]'); page.wait_for_timeout(300)
    check('/key/swarm/psk/1.0.0/' in page.input_value('textarea[readonly]') and 'zen2' in [b['name'] for p_, b in STATE['posts'] if p_ == '/api/v1/station/planes'], 'fonder une constellation montre la swarm.key créée, à copier')
    # Démarrage automatique : une unité qui lance encore « serve » est dite en désaccord ; sur « planes », la station peut redémarrer.
    page.goto(f'http://127.0.0.1:{port}/#/planes'); page.wait_for_selector('table'); page.wait_for_timeout(300)
    check('lance encore « serve »' in page.inner_text('main') and '/usr/bin/astroportd planes' in page.inner_text('main'), 'planes : le désaccord du démarrage automatique est dit, avec la commande à copier')
    check(page.locator('main button:has-text("Redémarrer la station")').count() == 0, 'planes : pas de redémarrage proposé tant que l\'unité lance serve')
    page.click('button:has-text("Basculer le service")'); page.wait_for_timeout(150)
    check('réécrite' in page.inner_text('main') and not any(p_ == '/api/v1/station/planes/service' for p_, _ in STATE['posts']), 'planes : le bouton de bascule explique au premier clic sans rien changer')
    page.click('button:has-text("Confirmer ?")'); page.wait_for_timeout(500)
    check(any(p_ == '/api/v1/station/planes/service' for p_, _ in STATE['posts']) and 'lance encore' not in page.inner_text('main'), 'planes : la bascule confirmée fait disparaître le désaccord')
    page.reload(); page.wait_for_selector('table'); page.wait_for_timeout(300)
    check('toutes les constellations sont lancées' in page.inner_text('main') and page.locator('main button:has-text("Redémarrer la station")').count() == 1, 'planes : unité sur planes : état dit et redémarrage proposé')
    page.click('button:has-text("Redémarrer la station")'); page.wait_for_timeout(150)
    check('indisponible quelques secondes' in page.inner_text('main') and not any(p_ == '/api/v1/station/restart' for p_, _ in STATE['posts']), 'planes : le premier clic explique sans redémarrer')
    page.click('button:has-text("Confirmer ?")'); page.wait_for_timeout(300)
    check(any(p_ == '/api/v1/station/restart' for p_, _ in STATE['posts']), 'planes : le second clic demande le redémarrage')
    # BRO : masqué tant que le MULTIPASS n'a déclaré ni Ollama ni clé d'API ; la déclaration (Mon compte) le fait apparaître.
    STATE['ai'] = False
    page.goto(f'http://127.0.0.1:{port}/#/devices'); page.reload(); page.wait_for_selector('#ai-url'); page.wait_for_timeout(300)
    check(page.locator('nav a[data-r=bro]').is_hidden(), "sans IA déclarée, l'onglet BRO n'apparaît pas")
    page.goto(f'http://127.0.0.1:{port}/#/bro'); page.wait_for_selector('#ai-url'); page.wait_for_timeout(300)
    check(page.evaluate('location.hash') == '#/devices?card=ai', "ouvrir #/bro sans IA renvoie vers Moi › Appareils, carte de l'IA")
    page.fill('#ai-url', 'http://127.0.0.1:11434'); page.click('section:has(#ai-url) button:has-text("Enregistrer")'); page.wait_for_selector('#ai-url'); page.wait_for_timeout(500)
    saved = [b for p_, b in STATE['posts'] if p_ == '/api/v1/me/ai']
    check(saved and saved[-1]['ollama_url'] == 'http://127.0.0.1:11434' and 'api_key' not in saved[-1], "l'adresse d'Ollama est enregistrée, sans clé inventée")
    check(page.locator('a.fab[href="#/bro"]').is_visible(), "une fois l'IA déclarée, le bouton ✨ de l'assistant apparaît")
    # Bibliothèque : fil du temps jour par jour, carte du monde hors ligne avec ses groupes, doublons sans options superflues.
    page.goto(f'http://127.0.0.1:{port}/#/library'); page.wait_for_selector('#lib-tab-time'); page.wait_for_timeout(700)
    check(page.locator('.stream h3.sticky').count() >= 1 and page.locator('.stream h4').count() >= 2, "l'affichage sur le temps groupe les photos par jour : " + str(page.locator('.stream h4').all_inner_texts()[:2]))
    # Sélection de vignettes (tout sélectionner, quart de tour) et visionneuse à boutons précédent/suivant.
    page.click('button:has-text("Sélectionner")'); page.locator('.stream .tile >> nth=0').click(); page.locator('.stream .tile >> nth=1').click()
    check('2 sélectionnée(s)' in page.inner_text('main'), 'deux vignettes cochées : le compte est affiché')
    page.click('button[aria-label="Tourner vers la droite"]'); page.wait_for_timeout(400)
    rot = [b for p_, b in STATE['posts'] if p_ == '/api/v1/ucloud/rotate']
    check(rot and rot[-1]['turn'] == 1 and len(rot[-1]['paths']) == 2, 'le quart de tour part avec les deux photos choisies')
    page.click('button:has-text("Tout sélectionner")'); check(page.locator('.tile.sel').count() >= 3, 'tout sélectionner coche les vignettes visibles')
    page.click('button:has-text("Terminer la sélection")'); page.locator('.stream .tile >> nth=0').click(); page.wait_for_selector('#view[open]'); page.wait_for_timeout(300)
    check(page.locator('#view-n').is_visible() and page.locator('#view-t').inner_text().startswith('1 / '), 'la visionneuse propose suivante et précédente avec le rang')
    page.click('#view-n'); page.wait_for_timeout(300); check(page.locator('#view-t').inner_text().startswith('2 / '), 'suivante passe à la photo d\'après')
    check(page.locator('#view-t button[aria-label="Tourner vers la gauche"]').count() == 1, 'la visionneuse permet de tourner la photo')
    page.click('#view-x')
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
    page.goto(f'http://127.0.0.1:{port}/#/people/tri'); page.wait_for_selector('.pe-busy'); page.wait_for_timeout(300)
    check(page.locator('.pe-busy').is_visible() and page.get_attribute('.pe-stage', 'aria-busy') == 'true', 'le sablier est affiché pendant le chargement de la table de tri')
    page.wait_for_timeout(2200)
    check(not page.locator('.pe-busy').is_visible(), 'le sablier disparaît quand la table est prête')
    STATE['delay'] = 0
    # Table de tri : supprimer les photos des visages sélectionnés (confirmation avec le compte exact).
    page.goto(f'http://127.0.0.1:{port}/#/people/tri'); page.wait_for_selector('canvas.pe-board'); page.wait_for_timeout(700)
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
    page.emulate_media(color_scheme='light'); page.goto(f'http://127.0.0.1:{port}/#/account'); page.wait_for_selector('#ac-name'); page.wait_for_selector('#style')
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
