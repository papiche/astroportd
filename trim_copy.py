#!/usr/bin/env python3
"""Allège une COPIE du dépôt avant de la donner à un assistant (appelé par cpcode.sh ; ne touche jamais au dépôt).

Usage : trim_copy.py <dossier> [--lean] [--only zone1,zone2] [--tests]
        trim_copy.py --zones          (liste les zones)

--lean  retire les commentaires `///` et `//` des .rs ; garde les en-têtes `//!`, qui disent le rôle de chaque module.
--only  garde en entier le code des zones demandées. Ailleurs, un .rs devient son **plan** (en-tête `//!`, signatures
        publiques avec la première ligne de leur doc, corps retirés) et les fichiers web disparaissent. Restent toujours
        en entier : Cargo.toml, CLAUDE.md, schema.sql, src/lib.rs, src/serve.rs (l'assemblage de la station).
"""
import re
import sys
from pathlib import Path

WEB = 'crates/astro-ui/web/'


def web(*names: str) -> list[str]:
    return [WEB + n for n in names]


def src(*names: str) -> list[str]:
    return [f'src/{n}.rs' for n in names]


# Une entrée est un préfixe de chemin relatif : un dossier (fini par /) ou un fichier.
ZONES: dict[str, tuple[str, list[str]]] = {
    'p2p': ('maillage, synchronisation, constellation, relais, tunnels', [
        'crates/astro-p2p/', 'crates/astro-sync/',
        *src('mesh_admin', 'mesh_outbox', 'swarm_cards', 'beacon', 'p2p_relay', 'tunnel_policy', 'constellation', 'captains',
             'plane', 'sponsor', 'forwards', 'geo_lease', 'integrity'), 'src/serve/p2p.rs',
        *web('m-mesh.js', 'm-constellation.js', 'm-planes.js', 'm-replication.js', 'm-replication.css', 'm-map.js', 'm-world.js',
             'stations.html', 'stations.js', 'stations.css'),
    ]),
    'web': ('interface web, API, cockpit, domaine et HTTPS, inscription', [
        'crates/astro-ui/',
        *src('ui_backend', 'station_domain', 'welcome', 'first_run', 'register', 'console', 'public_site', 'station_admin', 'station'),
        'src/serve/ui.rs', 'src/serve/admin.rs',
    ]),
    'dav': ('uCloud : WebDAV, CardDAV, CalDAV, partages, sauvegardes, médias', [
        'crates/astro-dav/',
        *src('sharing', 'share_inbox', 'webdav_pull', 'private_backup', 'library', 'organize', 'trash', 'media', 'upload_worker',
             'address_book', 'fetch', 'profile_image', 'legacy_ucloud', 'dnslink'), 'src/serve/tasks.rs',
        *web('m-library.js', 'm-photos.js', 'm-photos.css', 'm-share.js', 'm-sync.js', 'm-contacts.js', 'm-organize.js',
             'm-sources.js', 'm-publish.js', 'm-send.js', 'm-book.css'),
    ]),
    'eco': ('économie ẐEN, Ğ1/Duniter, billets, coopérative, énergie', [
        'crates/astro-economy/', 'crates/astro-duniter/', 'crates/astro-g1-runtime/',
        *src('funds', 'billet', 'settlement', 'hosting_fee', 'coop_wallets', 'coop_config', 'zen_react', 'member_primo', 'energy',
             'duniter_nodes', 'charter_accept', 'solar_cron'),
        *web('m-billets.js', 'm-billets.css', 'm-coop.js', 'm-flow.js', 'm-flow.css', 'claim.html', 'claim.js'),
    ]),
    'identity': ('identités, clés, Y-Level, MULTIPASS, init, invitations', [
        'crates/astro-lifecycle/', 'crates/astro-crypto/', 'crates/astro-core/',
        *src('identity', 'identity_links', 'person_keys', 'primal_init', 'rotate', 'init', 'account', 'invites', 'doctor',
             'intrusion_guard', 'main'),
        *web('m-account.js', 'm-account.css', 'm-who.js', 'invite.html', 'invite.js', 'invite.css', 'restore.html', 'restore.js',
             'register.html', 'register.js', 'register.css'),
    ]),
    'nostr': ('relai Nostr, base, chat, présence, visio (STUN/TURN)', [
        'crates/astro-relay/', 'crates/astro-nostr-proto/', 'crates/astro-db/',
        *src('chat', 'bridge', 'presence', 'stun', 'turn'),
        *web('m-chat.js', 'm-chat.css', 'm-call.js', 'm-call.css', 'm-nostr.js'),
    ]),
    'ia': ('BRO, FaceCloud, extensions', [
        'crates/astro-bro/', 'crates/astro-facecloud/', 'crates/astro-plugins/', 'plugins/',
        *src('member_ai', 'agents', 'assist'), 'src/serve/face.rs',
        *web('m-bro.js', 'm-people.js', 'm-people.css', 'm-review.js', 'm-card.js', 'scan.html', 'scan.js', 'scan.css',
             'scan-rules.js', 'scan-i18n.js', 'scan-lang-en.js', 'scan-lang-es.js'),
    ]),
}

ALWAYS = ['Cargo.toml', 'CLAUDE.md', 'crates/astro-db/src/schema.sql', 'src/lib.rs', 'src/serve.rs']
ALWAYS_SUFFIX = ['/Cargo.toml']

COMMENT = re.compile(r'^[ \t]*//(?!!)')
ITEM = re.compile(r'^(?P<ind> {0,4})(?:pub(?:\([^)]*\))? )(?:(?:async|const|unsafe|extern "C") )*(?P<kw>fn|struct|enum|trait|type|const|static|mod|use)\b')
IMPL = re.compile(r'^(?:unsafe )?impl\b')


def lean(text: str) -> str:
    return ''.join(l for l in text.splitlines(keepends=True) if not COMMENT.match(l))


def outline(text: str) -> str:
    """Plan d'un fichier Rust : en-tête `//!`, `impl …` et items publics, corps remplacés par `{ … }`."""
    lines = text.split('\n')
    out = [l for l in lines if l.startswith('//!')]
    if out:
        out.append('')
    i, n = 0, len(lines)
    while i < n:
        l = lines[i]
        if ITEM.match(l) or IMPL.match(l):
            # Première ligne de la doc juste au-dessus de l'item.
            j = i - 1
            while j >= 0 and re.match(r'^\s*#\[', lines[j]):
                j -= 1
            k = j
            while k >= 0 and lines[k].lstrip().startswith('///'):
                k -= 1
            if k < j:
                out.append(lines[k + 1])
            sig = [l]
            while not re.search(r'[{;]\s*$', sig[-1]) and '{' not in sig[-1] and i + 1 < n and len(sig) < 12:
                i += 1
                sig.append(lines[i])
            s = '\n'.join(sig)
            if '{' in s and not s.rstrip().endswith(';'):
                s = s[:s.index('{')].rstrip() + ' { … }'
            out.append(s)
        i += 1
    return '\n'.join(out) + '\n'


def matches(rel: str, prefixes: list[str]) -> bool:
    return any(rel == p or (p.endswith('/') and rel.startswith(p)) for p in prefixes)


def main() -> None:
    args = sys.argv[1:]
    if args == ['--zones']:
        for name, (what, _) in ZONES.items():
            print(f'  {name:<9} {what}')
        return
    root = Path(args[0])
    do_lean, tests = '--lean' in args, '--tests' in args
    only: list[str] = []
    if '--only' in args:
        only = [z for z in args[args.index('--only') + 1].split(',') if z]
        unknown = [z for z in only if z not in ZONES]
        if unknown:
            sys.exit(f"zone inconnue : {', '.join(unknown)} (zones : {', '.join(ZONES)})")
    keep = [p for z in only for p in ZONES[z][1]]
    stems = {Path(p).stem for p in keep if p.startswith('src/') and p.endswith('.rs')}
    for f in sorted(p for p in root.rglob('*') if p.is_file()):
        rel = f.relative_to(root).as_posix()
        if only and not (rel in ALWAYS or any(rel.endswith(s) for s in ALWAYS_SUFFIX) or matches(rel, keep)):
            if rel.startswith('tests/'):
                # Tests d'intégration : gardés seulement s'ils portent le nom d'un module de la zone.
                if not (tests and f.stem in stems):
                    f.unlink()
                    continue
            elif '/tests/' in rel or rel.startswith(WEB) or not rel.endswith('.rs'):
                f.unlink()
                continue
            else:
                f.write_text(outline(f.read_text(encoding='utf-8')), encoding='utf-8')
                continue
        if do_lean and rel.endswith('.rs'):
            f.write_text(lean(f.read_text(encoding='utf-8')), encoding='utf-8')


if __name__ == '__main__':
    main()
