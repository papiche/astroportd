## astroportd 0.5.0-beta

Cette version fait de la **clé de secours** un secret qu'on peut perdre sans danger, et ajoute l'**agenda** : « Agenda » du téléphone se synchronise avec la station, comme « Contacts ».

### Clé de secours en 3 parts (remplace la carte complète)
- Le secret du compte (SALT et PEPPER) est découpé en **3 parts, dont 2 suffisent** (partage de Shamir). **P1** va au membre (QR imprimable `astro-share:v1|…`), **P2** reste sur la station (`<compte>/.ssss/2`, `0600`), **P3** est confiée d'elle-même, chiffrée pour chacun, aux amis qui hébergent la sauvegarde. Une part seule ne révèle rien : un QR volé ou photographié ne donne pas accès au compte.
- **« J'appelle un ami »** : sur la nouvelle station (`/restore`, ou `astroportd multipass recover -`), on saisit ou scanne P1 et la part que l'ami montre dans *Compte › Sauvegarde privée* (texte et QR). La station rétablie recalcule les trois parts d'origine : le P1 imprimé et le P3 des amis restent valables.
- `astroportd multipass card <email> [--part 1|2|3] [--svg]` réaffiche une part ; un compte plus ancien reçoit son découpage à la première demande.
- **Le SALT et le PEPPER ne sont plus affichés** à l'inscription (page, fichier `.txt`, texte du terminal ; `--json` les garde). Le `nsec` reste affiché (clients Nostr à extension de navigateur). L'**ancienne carte complète** (`astro-secret:v1|…`) reste acceptée à la restauration.
- Tant qu'aucun ami n'héberge la sauvegarde, la part P3 n'existe que sur la station.

### Agenda (CalDAV) et synchronisation incrémentale
- **CalDAV** (RFC 4791) : agenda `/dav/calendars/<email>/default/`, événements, tâches et notes `.ics` **chiffrés** dans `/Calendrier` de votre uCloud (donc répliqués avec la sauvegarde, jamais publiés sur Nostr). Découverte `/.well-known/caldav`, `calendar-multiget`, `calendar-query`, validation iCalendar (un seul `UID` par objet, 512 Ko au plus).
- **`sync-collection`** (RFC 6578) pour l'agenda **et** le carnet d'adresses : un journal des changements (`dav_changes`, alimenté par des triggers SQL, suppressions comprises) ne renvoie au téléphone que ce qui a changé ; un jeton plus ancien que 30 jours renvoie le client à une synchronisation complète.
- Limites : `calendar-query` renvoie tout l'agenda sans filtre de dates ni développement des récurrences (le client s'en charge) ; pas d'invitations entre participants ; un seul agenda par compte. **Non testé avec de vrais clients** (DAVx⁵, iOS, Thunderbird) : tests réels à faire.

### Déménagement
- Le dossier `.moved/` d'un compte déménagé (il contient encore ses clés) est **effacé après 30 jours** par la nuit de 20h12.

### À savoir
- Le smoke navigateur complet (`tests/browser/smoke_ui.py`) échoue sur `input[name=fo-mode]` depuis avant cette version ; les pages inscription et restauration passent avec ce bloc sauté.
- La restauration par plusieurs amis (k parts sur n) reste à venir.
