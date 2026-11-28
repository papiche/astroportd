"""Test de fumée navigateur : sur quelle UPlanet suis-je, monnaie renommée, champs que le navigateur ne doit pas remplir.

- ORIGIN : style Kraft imposé (le choix du style disparaît), « ORIGIN » à côté du logo.
- UPlanet privée renommée (`/api/v1/look`) : son nom dans l'en-tête, « Ẑen » et « Ẑ » remplacés partout, Kraft absent du choix.
- Aucun champ hors du formulaire de connexion n'est un `type=password` ni un identifiant pour le navigateur (Chrome y
  proposait le compte de la station : dossier d'un appareil, clé d'API).
- Configuration coopérative : clé Pl@ntNet propre à la station ; Constellations : domaine public par plan et certificats HTTPS.
Ordinateur et téléphone (400 px), aucune erreur de console tolérée.

    ~/.astro/bin/python tests/browser/smoke_look.py [dossier-des-captures]
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

ZEN = dict(origin=False, name='Pays Basque', currency='Sol', symbol='☉', style='sobre')
with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in ((1280, 'pc'), (400, 'tel')):
        STATE.pop('look', None)
        ctx = br.new_context(viewport=dict(width=width, height=820), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        # ORIGIN : Kraft imposé, nom affiché (masqué sur téléphone, où la place manque).
        page.goto(f'http://127.0.0.1:{port}/#/'); page.wait_for_selector('main h2, main .card', state='attached'); page.wait_for_timeout(500)
        check(page.evaluate('document.documentElement.dataset.style') == 'kraft', f'[{tag}] ORIGIN est en Kraft')
        check(page.locator('#style').is_hidden(), f'[{tag}] ORIGIN : pas de choix de style')
        check(page.locator('.brand .up').inner_text() == 'ORIGIN', f'[{tag}] « ORIGIN » à côté du logo')
        check(page.locator('.brand .up').is_visible() == (width > 520), f'[{tag}] le nom se cache seulement sur petit écran')
        page.screenshot(path=str(OUT / f'look-origin-{tag}.png'))
        # UPlanet privée renommée : la réponse est gardée et appliquée au chargement suivant.
        STATE['look'] = ZEN
        page.reload(); page.wait_for_timeout(800)
        check(page.evaluate('document.documentElement.dataset.style') == 'sobre', f'[{tag}] style par défaut de la coopérative (Sobre)')
        check(page.evaluate("[...document.querySelectorAll('#style option')].filter(o => !o.hidden).map(o => o.value).join()") == 'aurore,sobre', f'[{tag}] Kraft absent du choix hors ORIGIN')
        check(page.locator('.brand .up').inner_text() == 'Pays Basque', f'[{tag}] nom de l\'UPlanet dans l\'en-tête')
        page.goto(f'http://127.0.0.1:{port}/#/money'); page.wait_for_timeout(900)
        txt = page.inner_text('main')
        check('Ẑen' not in txt and ' Ẑ' not in txt and '☉' in page.inner_text('header'), f'[{tag}] « Ẑen » et « Ẑ » remplacés dans Mes Ẑen et dans le solde de l\'en-tête')
        page.screenshot(path=str(OUT / f'look-zen-money-{tag}.png'), full_page=True)
        # Appareils : le dossier n'est pas un identifiant pour le navigateur.
        page.goto(f'http://127.0.0.1:{port}/#/devices'); page.wait_for_timeout(900)
        root = page.locator('#dv-root')
        if root.count():
            check(root.get_attribute('autocomplete') == 'off' and root.get_attribute('data-1p-ignore') is not None, f'[{tag}] dossier d\'un appareil : pas de remplissage par le navigateur')
        check(page.locator('main input[type=password]').count() == 0, f'[{tag}] aucun type=password hors connexion (Appareils)')
        # Configuration coopérative : section Monnaie et apparence, clé Pl@ntNet de la station.
        page.goto(f'http://127.0.0.1:{port}/#/coop'); page.wait_for_selector('form fieldset'); page.wait_for_timeout(300)
        page.evaluate("document.querySelectorAll('main details').forEach(d => d.open = true)")
        check(page.locator('main input[type=password]').count() == 0 and page.locator('main input.secret').count() >= 1, f'[{tag}] secrets coopératifs masqués sans type=password')
        page.screenshot(path=str(OUT / f'look-coop-{tag}.png'), full_page=True)
        # Constellations : domaine public de chaque plan, certificats HTTPS.
        page.goto(f'http://127.0.0.1:{port}/#/planes'); page.wait_for_selector('main table'); page.wait_for_timeout(300)
        check('station.exemple.org' in page.inner_text('main table'), f'[{tag}] domaine public d\'ORIGIN dans le tableau')
        check(page.locator('main h3:text-is("Certificats HTTPS")').count() == 1, f'[{tag}] carte Certificats HTTPS')
        page.locator('button[aria-label^="Régler le domaine de « zen1 »"]').click()
        page.locator('input[aria-label^="Domaine public de « zen1 »"]').fill('zen1.exemple.org')
        check(page.locator('button:text-is("Vérifier et enregistrer")').is_visible(), f'[{tag}] le domaine se saisit dans la ligne du plan')
        page.screenshot(path=str(OUT / f'look-planes-{tag}.png'), full_page=True)
        # Aucune page ne déborde en largeur sur téléphone.
        if width == 400:
            check(page.evaluate('document.documentElement.scrollWidth') <= 410, f'[{tag}] pas de défilement horizontal ({page.evaluate("document.documentElement.scrollWidth")} px)')
        ctx.close()
    br.close()
srv.shutdown()
errors = [e for e in errors if '404' not in e]
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
sys.exit(1 if failures else 0)
