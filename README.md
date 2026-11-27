# MagicPlus

Presse-papiers partagé, transfert de fichiers et **discussions** (à deux ou en groupe) **directement
entre les ordinateurs d'un même réseau local** (Mac ↔ Windows), chiffrés de bout en bout. Pas de
serveur, pas de compte, pas d'Internet. Application 100 % Rust.

- **macOS** : interface **AppKit native** (vrais contrôles macOS, écrits en Rust avec `objc2`).
- **Windows** (et Linux) : interface **Slint** au style Fluent (Windows 11).
- Les deux interfaces utilisent **le même moteur** : un Mac et un PC se parlent normalement.
- Code et versions publiées : <https://github.com/Ywn509zz/magicplus>. Les apps installées y
  cherchent seules leurs mises à jour (usage d'Internet désactivable). La recherche de GIF (GIPHY,
  avec une clé d'API à fournir) est l'autre usage d'Internet, seulement quand on l'ouvre.

---

## Résumé pour un assistant IA (à lire en premier)

| Question | Réponse |
| --- | --- |
| Langage, édition | Rust 2021, une seule crate `magicplus` : une bibliothèque `magicplus_core` (`src/lib.rs`) et un exécutable (`src/main.rs`). |
| Où est la logique | `src/engine.rs` (état, association, presse-papiers, découverte), `src/transfer.rs` (fichiers reprenables), `src/chat.rs` (discussions et groupes), `src/secure.rs` (canal chiffré Noise) et `src/net.rs` (UDP/TCP). Aucune dépendance à l'interface. |
| Où est l'interface | macOS : `src/ui_mac.rs` + `src/ui_mac_chat.rs` (AppKit). Windows/Linux : `src/ui_slint.rs` + `src/ui_slint/chat.rs`, avec `ui/app.slint`, `ui/chat.slint`, `ui/theme.slint`. Choix par `#[cfg]` dans `src/main.rs`. |
| Dépendances par plateforme | `Cargo.toml` : `slint` seulement hors macOS (ou avec `--features slint-on-mac`), `objc2*` seulement sur macOS. `build.rs` ne compile `ui/app.slint` que pour Slint. |
| Comment l'interface lit l'état | Toutes les 150 ms : si `engine.take_dirty()` est vrai, elle relit `engine.snapshot()` et se redessine. Les discussions ont leur propre compteur, `engine.chat_revision()` : la page ne se refait que s'il change. Les actions appellent les méthodes publiques de `Engine`. |
| Règle d'or | Le moteur ne connaît pas l'interface. Tout ce qui doit s'afficher passe par `Snapshot` ; tout changement d'état appelle `touch()` (ou `set_status`). Ne jamais bloquer le fil de l'interface (réseau, résolution DNS, gros fichiers → autre fil). |
| Tester | `cargo test` (unitaires + `tests/e2e.rs` et `tests/chat.rs` : des moteurs complets qui se découvrent, s'associent et échangent). |
| Vérifier Windows depuis un Mac | `rustup target add x86_64-pc-windows-msvc` puis `cargo check --target x86_64-pc-windows-msvc` (vérifie le code, ne produit pas d'exécutable). Pour **voir** l'interface Windows sur le Mac : `--features slint-on-mac` (voir « Variables d'environnement »). |
| Publier une version | Changer `version` dans `Cargo.toml`, valider, puis `git tag -a vX.Y.Z -m "notes"` et `git push origin main vX.Y.Z`. GitHub Actions (`.github/workflows/versions.yml`) compile Windows + macOS et crée la version ; les apps la proposent seules (`src/update.rs`). |
| Langue | Textes de l'interface, messages et commentaires en **français**. |
| Pourquoi AppKit sur Mac | Slint (OpenGL comme rendu logiciel) réagissait très lentement aux clics sur un Mac sans accélération Metal (MacBook Pro 2011 sous OpenCore Legacy Patcher), alors qu'AppKit y reste fluide. Ne pas remettre Slint ni SwiftUI sur macOS. |
| Bluetooth | Pas de BLE (le Mac de 2011 n'en a pas). Réseau local d'abord (Wi-Fi, Ethernet, adresse IP) ; sans réseau commun, Bluetooth classique RFCOMM avec les ordinateurs appairés au système (`src/bluetooth/`), sous le même canal chiffré. |

---

## Compiler

Prérequis communs : [Rust](https://rustup.rs) (stable, testé avec 1.98).

### Windows

Le plus simple : télécharger `MagicPlus-windows-x64.exe` dans la dernière version publiée
(<https://github.com/Ywn509zz/magicplus/releases/latest>) et le lancer ; il se met ensuite à jour
tout seul. Pour compiler soi-même :

1. Installer **Rust** : <https://rustup.rs> (accepter la chaîne d'outils `stable-x86_64-pc-windows-msvc`).
2. Installer **Visual Studio Build Tools 2022** : <https://visualstudio.microsoft.com/fr/visual-cpp-build-tools/>,
   charge de travail **« Développement Desktop en C++ »** (inclut le SDK Windows). Rien d'autre :
   ni Node, ni WebView2.
3. Dans PowerShell, dans le dossier du projet (celui qui contient `Cargo.toml`) :

   ```powershell
   cargo build --release
   ```

   L'exécutable est `target\release\magicplus.exe` (aucune fenêtre de console en mode release).
   La première compilation prend plusieurs minutes. `cargo run --release` compile et lance.
4. Au premier lancement, Windows demande l'accès au réseau : autoriser MagicPlus pour les
   **réseaux privés**. Sans cela la machine n'est ni détectée ni joignable. Le réseau Wi-Fi doit être
   déclaré « privé » dans les paramètres Windows (pas « public »).

### macOS

Le plus simple : télécharger `MagicPlus-macos.zip` dans la dernière version publiée, le décompresser
et glisser `MagicPlus.app` dans Applications (au premier lancement : clic droit → Ouvrir, l'app
n'étant signée que localement). Pour compiler soi-même :

1. Installer Rust (`rustup`). Les outils en ligne de commande d'Xcode sont nécessaires pour l'éditeur
   de liens (`xcode-select --install`).
2. Dans le dossier du projet :

   ```bash
   packaging/macos/build-app.sh                          # produit target/release/MagicPlus.app
   ditto target/release/MagicPlus.app /Applications/MagicPlus.app   # installe (ou glisser dans Applications)
   ```

   Le script compile la version release, assemble `MagicPlus.app` (`packaging/macos/Info.plist`,
   icône `packaging/macos/icon.png` convertie en `.icns`) et la signe localement (`codesign -s -`).
   Pour essayer sans empaqueter : `cargo run --release` (les autorisations macOS, comme
   l'Accessibilité, vont alors au terminal et non à MagicPlus).

### Linux (non testé)

Même interface que Windows (Slint). Il faut les paquets de développement X11/Wayland usuels ;
`xdotool` sert au raccourci « copier et envoyer ».

### Tests

```bash
cargo test
```

- Unitaires : canal Noise (aller-retour, mauvaise clé, altération, rejeu, clés neuves à chaque
  connexion, contenu illisible sur le fil), noms de fichiers dangereux, résolution d'adresses,
  Markdown des messages (`src/markdown.rs`), ordre et complétude des événements de discussion.
- Interface Slint : `cargo test --features slint-on-mac --target-dir target/slint-mac --bin magicplus`
  vérifie que tout message, même piégé, est accepté par l'analyseur Markdown de Slint.
- Mesures (ignorées par défaut) : `cargo test --release --lib debit_du_chiffrement -- --ignored --nocapture`
  et `cargo test --release --test e2e debit_d_un_transfert -- --ignored --nocapture`.
- `tests/e2e.rs` : deux moteurs dans le même processus, sur des ports dédiés (47100 et suivants) :
  découverte, association, fichiers sur disque (sans écrasement), liaison à sens unique qui reste en
  ligne, refus expliqué d'un appareil non associé puis livraison une fois associé, texte du
  presse-papiers, **reprise d'un transfert coupé** (à l'octet près), **reprise après redémarrage du
  destinataire**, annulation, connexion par adresse IP entre deux moteurs isolés, appareil d'une
  autre version signalé (plus ancienne ou plus récente).
- `tests/chat.rs` : discussion à deux (réponse, modification, réaction, accusé de lecture, « écrit… »,
  suppression), messages **complets et dans l'ordre** après une longue absence du destinataire,
  groupe de trois membres présentés par le créateur (sans association entre eux), **membres ajoutés
  par n'importe quel membre et après la création**, **demandes d'ami** (acceptée, croisée, refusée),
  pièces jointes et
  autocollants, profils (reçus par un associé et par un simple membre de groupe), **canevas** à
  trois (frappes simultanées fusionnées, arrivée en moins de 3 s, versions nommées, restauration,
  renommage, suppression). `donnees_de_demo` (ignoré) fabrique des discussions de démonstration dans
  `MAGICPLUS_DEMO_DIR` pour les captures d'écran.
- `Config::fail_after_chunks` et `Config::retry_delay` servent aux tests de coupure et de reprise.

---

## Utilisation

1. Ouvrir MagicPlus sur les deux machines, sur le même réseau. Elles apparaissent dans **Appareils**.
   Sinon (réseau d'entreprise, Wi-Fi invité…), saisir l'adresse IP de l'autre machine dans
   **Connexion par adresse IP** (l'adresse de chaque machine y est affichée ; celles des cartes
   virtuelles VMware, Hyper-V, WSL… sont mises à part, elles ne servent pas aux autres appareils).
   Une seule machine suffit.
   **Sous Windows**, si la page Appareils affiche « Le pare-feu de Windows bloque MagicPlus », cliquer
   **Autoriser MagicPlus** (Windows demande l'accord administrateur) : sans cela, le PC peut voir les
   autres sans qu'eux puissent le joindre (« Détecté · hors ligne » chez eux).
2. Sur **chacune**, cliquer **Associer** et vérifier que l'empreinte affichée est identique à celle de
   l'autre écran (ligne « cet appareil »).
   Tant qu'un appareil ne vous a pas associé, ses envois sont refusés : il est alors marqué
   « Demande d'association », et l'expéditeur voit pourquoi son envoi n'est pas passé.
3. **Presse-papiers** : bouton « Envoyer le presse-papiers », ou raccourcis globaux
   `Cmd/Ctrl + Shift + Option/Alt + C` (copie la sélection puis l'envoie) et `… + V` (remet le dernier
   texte reçu dans le presse-papiers). Par défaut, le texte reçu est **mis automatiquement dans le
   presse-papiers** : on copie sur une machine et on colle directement sur l'autre.
4. **Transferts** : choisir des fichiers ; ils partent vers tous les appareils associés joignables et
   arrivent dans Téléchargements (ou le dossier choisi dans **Paramètres**). Un transfert coupé
   (Wi-Fi, veille, ordinateur éteint, app quittée) **reprend tout seul là où il s'était arrêté**,
   pendant 48 h par défaut (réglable dans Paramètres). Bouton « Annuler » pour l'abandonner.
5. **Discussions** : bouton « Discuter » d'un appareil associé (page Appareils) ou « + » de la page
   Discussions (« Discuter avec … », « Nouveau groupe… »). Entrée envoie, Maj+Entrée va à la ligne,
   Échap annule une réponse ou une modification, ↑ dans une zone vide modifie son dernier message.
   Coller (Cmd/Ctrl+V) une image ou des fichiers les joint au message (15 au plus, 1 Go chacun) ;
   bouton trombone pour en choisir. Survol (Windows) ou bouton « … » (Mac) d'un message : réagir,
   répondre, modifier, supprimer ; clic droit : même menu. Bouton autocollant : envoyer un autocollant
   ou un GIF de sa bibliothèque, ou en créer un à partir d'une image.
   **Groupes** : bouton « … » en haut de la conversation : « Ajouter des membres… » (pour **tout**
   membre, parmi ses appareils associés), liste des membres avec « Ajouter en ami » pour ceux que l'on
   n'a pas associés ; la demande arrive dans la page Appareils de l'autre (Accepter / Refuser).
6. **Arrière-plan et notifications** : fermer la fenêtre ne quitte pas MagicPlus. Il reste actif
   (icône dans la barre des menus sur Mac, près de l'horloge sous Windows), donc **joignable par les
   autres appareils**, et affiche une **notification du système** à chaque message ou demande d'ami
   reçus (sauf si la conversation est déjà sous vos yeux) ; un clic ouvre la conversation. Le nombre
   de messages non lus s'affiche sur l'icône (et sur celle du Dock). Pour le quitter : menu de
   l'icône → Quitter MagicPlus (ou Cmd+Q sur Mac). **Paramètres → Démarrage et notifications** :
   « Lancer MagicPlus à l'ouverture de la session » (il démarre alors en arrière-plan, sans fenêtre)
   et notifications activées ou non. Sur Mac, la première fois, macOS demande d'autoriser les
   notifications (l'app doit être dans le dossier Applications).
7. **Sans réseau commun (Bluetooth)** : appairez une fois les deux ordinateurs dans les réglages
   Bluetooth du système (Mac : Réglages Système → Bluetooth ; Windows : Paramètres → Bluetooth et
   appareils → Ajouter un appareil), comme pour un casque. Ensuite, quand ils n'ont pas de réseau en
   commun (pas de Wi-Fi, pas d'Internet, réseau qui isole les appareils), MagicPlus les relie tout seul
   en Bluetooth : l'appareil apparaît « via Bluetooth » dans Appareils, et discussions, demandes d'ami,
   presse-papiers et fichiers passent par là (plus lentement : environ 100 Ko/s). Il faut toujours
   **associer** les appareils dans MagicPlus (empreintes), ou être dans un groupe commun.
8. **Profils** : Paramètres → Mon profil. En choisissant une photo ou une bannière, une fenêtre de
   **recadrage** règle le zoom et la partie gardée (jamais d'étirement ; un GIF animé reste animé).
   Le **profil en grand** (photo de quelqu'un → « Profil complet », ou bouton « Profil » dans
   Appareils) montre bannière, photo, pseudo, pronoms, présentation, groupes en commun, et propose
   « Envoyer un message » ou « Ajouter en ami ».
9. **Images, mentions, envois ciblés** : un clic sur une image ou un GIF l'affiche **en grand**.
   Taper « @ » propose les membres de la discussion (Entrée ou Tab : le premier) ; les mentions sont
   colorées et un message qui vous mentionne est surligné. « Envoyer des fichiers » et « Envoyer le
   presse-papiers » demandent **à quels appareils** envoyer quand plusieurs sont joignables. Le menu
   d'un message avec une image ou un GIF permet de l'ajouter à ses GIF et autocollants, de le copier
   (pour le coller ailleurs) ou de l'envoyer à un appareil ; hors ligne, le bouton GIF propose les GIF
   déjà envoyés.
10. **Mises à jour** : quand une nouvelle version est publiée, la barre d'état l'annonce et
   « Paramètres » porte une pastille. **Paramètres → Mises à jour → Télécharger et installer**, puis
   **Redémarrer et installer** : MagicPlus se remplace et redémarre. Au premier lancement de la
   nouvelle version, une fenêtre résume les **nouveautés** (tirées de `CHANGELOG.md`, intégré au
   programme ; à rouvrir depuis Paramètres → Mises à jour → Voir les nouveautés). **À chaque version,
   ajouter sa section en tête de `CHANGELOG.md`** (un test vérifie qu'elle correspond à `Cargo.toml`).

---

## Fonctionnement détaillé

### Vue d'ensemble

```
 Interface (AppKit ou Slint)
   │  appelle Engine::trust / send_files / send_clipboard / add_manual_address …
   │  relit Engine::snapshot() quand Engine::take_dirty() est vrai (toutes les 150 ms)
   ▼
 Engine (src/engine.rs) ── état partagé (Mutex), stockage JSON (src/store.rs)
   │
   ├─ fil « annonces »  : Hello UDP toutes les 2 s (src/net.rs)
   ├─ fil « écoute UDP »: reçoit les Hello, répond directement à chacun (sauf aux réponses)
   ├─ fil « écoute TCP »: une connexion entrante = un fil, trames chiffrées
   ├─ fil « ménage »    : chaque seconde, passe les appareils silencieux hors ligne
   └─ fil « planificateur » (src/transfer.rs) : lance et relance les envois en attente
```

### Découverte (UDP, port 45454)

- Message `Hello` en JSON : `{protocol: "magicplus/4", device: {id, name, platform, publicKey},
  tcpPort, discoveryPort, reply, appVersion}`. Les autres versions (`magicplus/1` Tauri, `/2` = 0.2, `/3` = 0.3,
  avant le canal Noise) sont ignorées : **toutes les machines doivent avoir la même version**.
- Toutes les 2 s, l'annonce part **sur chaque interface IPv4** (un socket lié à l'adresse de
  l'interface, vers son adresse de diffusion et vers 255.255.255.255). Indispensable sous Windows
  avec VPN/Hyper-V/WSL, où la route par défaut n'est pas forcément le bon réseau.
- Elle part aussi **directement** (unicast) vers les adresses saisies à la main
  (`Settings::manual_addresses`, résolues par le fil d'annonces, jamais par l'interface) et vers
  `Config::extra_targets` (tests).
- À **chaque** `Hello` reçu (au plus une fois par seconde et par appareil), on renvoie notre `Hello`
  directement sur son `discoveryPort`, marqué `reply: true` ; on ne répond jamais à une réponse
  (pas de boucle). Ainsi un appareil qui ne nous joint qu'en direct (adresse saisie à la main,
  diffusion filtrée par le réseau) reste en ligne des deux côtés. Avant la version 0.3, on ne
  répondait qu'au premier `Hello` : l'autre machine passait « hors ligne » au bout de 8 s.
- `appVersion` (depuis la 0.4) donne la version de l'app. Une annonce d'un **autre protocole**
  `magicplus/N` n'est pas utilisée, mais l'appareil est signalé (`Snapshot::other_versions`) : page
  Appareils et barre d'état indiquent son nom, son adresse, sa version (lue, ou déduite : protocole
  N = 0.N) et quelle machine mettre à jour. L'avertissement disparaît 20 s après sa dernière annonce
  ou dès qu'il s'annonce avec le bon protocole.
- L'empreinte n'est jamais lue dans le message : elle est recalculée à partir de la clé publique.
- Un appareil est « en ligne » s'il s'est annoncé depuis moins de 8 s ; un appareil non associé
  disparaît après 60 s de silence.
- Si un port est occupé au démarrage, l'app réessaie toutes les 3 s.
- **Pare-feu Windows** (`src/firewall.rs`). Quand Windows demande l'autorisation au premier
  lancement, il ne coche que « réseaux privés » ; sur un réseau classé « Public » (école, entreprise),
  ou après un refus (qui crée des règles de **blocage**, prioritaires), les annonces et connexions
  entrantes sont bloquées : le PC voit les autres mais aucun ne peut lui répondre. Au démarrage puis
  toutes les 15 min, l'app vérifie par PowerShell (sans droits administrateur) qu'aucune règle ne
  bloque son programme et qu'une règle l'autorise sur les réseaux actuels. Sinon, le bouton
  « Autoriser MagicPlus » lance **une** demande d'élévation (UAC) qui supprime les règles de blocage
  du programme et ajoute trois règles « MagicPlus » (programme, UDP 45454, TCP 45455) sur tous les
  profils. Une connexion impossible le rappelle dans son message d'erreur.
- **Cartes virtuelles** : les adresses des cartes VMware, VirtualBox, Hyper-V (`vEthernet`), WSL,
  Docker, `bridge100`… (reconnues à leur nom) sont affichées à part dans la page Appareils.

### Identité et association

- Au premier lancement : un identifiant (UUID v4) et une paire de clés Curve25519, enregistrés dans
  `identity.json` (droits 0600 sous Unix).
- Empreinte = les 19 premiers caractères du SHA-256 (base64) de la clé publique base64, `/` remplacés
  par `x`. L'utilisateur la compare à l'œil sur les deux écrans.
- Associer enregistre l'appareil **avec sa clé publique du moment** dans `trusted.json`. Ensuite, seule
  cette clé est utilisée. Si l'appareil annonce une autre clé, il est marqué « Clé modifiée », plus
  rien ne lui est envoyé, et il faut le ré-associer explicitement.
- L'association est à sens unique : chaque machine doit associer l'autre pour qu'elle accepte ses envois.
- **Noms** : chaque appareil a un nom d'ordinateur (`identity.name`) et un **pseudo** facultatif
  (`identity.pseudo`, 32 caractères), annoncé dans chaque `Hello` (`device.pseudo`) : les autres le
  voient dès la détection, avant même l'association, et le gardent dans `pseudos.json`. Chacun peut
  aussi donner un **surnom** local à un autre appareil (`Settings::aliases`, visible seulement chez
  lui). Nom affiché partout (appareils, discussions, profils, canevas) : surnom, sinon pseudo, sinon
  nom de l'ordinateur (`Engine::display_name`). Ce qui part vers les autres (message transféré) porte
  le nom **public** (pseudo ou nom de l'ordinateur), jamais un surnom.

### Chiffrement (TCP, port 45455) — `src/secure.rs`

Chaque connexion TCP est un canal **Noise** `Noise_IK_25519_ChaChaPoly_BLAKE2s` (le schéma de
WireGuard), via la bibliothèque `snow`. Aucun octet applicatif ne circule en clair, réponses comprises.

- **Poignée de main** (un aller-retour) : l'expéditeur connaît la clé publique du destinataire
  (mémorisée à l'association) ; chacun prouve qu'il détient sa clé secrète X25519 et des clés
  **éphémères** sont échangées. Le premier message contient l'identifiant de l'expéditeur ; le
  destinataire retrouve l'appareil associé **par la clé prouvée**, jamais par l'identifiant annoncé.
- **Confidentialité persistante** : les clés de session viennent des clés éphémères ; voler plus
  tard la clé secrète d'un appareil ne permet pas de déchiffrer un trafic enregistré.
- **Transport** : ChaCha20-Poly1305 avec un compteur par sens. Un message modifié, rejoué, supprimé
  ou réordonné ne se déchiffre pas : la connexion est coupée. Rejouer une ancienne poignée de main
  est inutile (nouvelle clé éphémère du destinataire à chaque fois).
- **Sur le fil** : messages Noise préfixés par leur longueur (`u16`, 65 535 octets au plus). Dans le
  flux déchiffré : chaque message applicatif = `u32 longueur` + contenu (4 Mo au plus).
- **Message applicatif** : `u32 taille de l'en-tête` + en-tête JSON (`protocol::Message`) + octets
  bruts (contenu d'un morceau de fichier). Les réponses (`protocol::Ack`) sont des messages JSON.
- **Pourquoi ChaCha20 plutôt qu'AES-GCM** : mesuré sur le Mac de 2011 (avec AES-NI), AES-GCM n'est
  que 20 % plus rapide ; ChaCha20 reste rapide et à temps constant sur toutes les machines, y compris
  les PC sans accélération AES. Débit mesuré : ~135 Mo/s pour chiffrer **et** déchiffrer sur un seul
  cœur de ce processeur, donc ~270 Mo/s par machine, au-dessus du Wi-Fi et de l'Ethernet gigabit.
- Un appareil non associé peut terminer la poignée de main (cela ne révèle rien), puis reçoit un
  refus chiffré `not-paired`. Une clé qui ne correspond pas fait échouer la négociation.

### Messages (`src/protocol.rs`)

| `kind` | Contenu | Effet |
| --- | --- | --- |
| `clipboard-text` | `id`, `text` (2 Mo max) | « Dernier texte reçu » ; copié dans le presse-papiers si `auto_copy_received`. |
| `file-offer` | `id`, `fileName`, `size`, `digest` (SHA-256 base64) | Ouvre ou reprend `partiels/<id>.part` ; répond tout de suite par l'offset. |
| `file-chunk` | `id`, `sequence` + octets (256 Ko max) | Doit arriver dans l'ordre ; écrit sur le disque, hachage au fil de l'eau. |
| `file-complete` | `id` | Vérifie taille et SHA-256, déplace le fichier dans le dossier de réception (sans écraser). |
| `file-cancel` | `id` | L'expéditeur abandonne : le récepteur supprime le fichier partiel. |
| `chat-events`, `chat-typing`, `chat-read` | conversation, événements (+ `group` pour un groupe) | Discussions (voir plus bas). |
| `profile`, `canvas-sync` | profil + images ; mise à jour Yjs | Profils et canevas (voir plus bas). |
| `friend-request` | `conversation` (groupe commun) | Demande d'ami ; réponse `Ack.data` = `accepted` ou `pending`. |
| `friend-answer` | `accepted` | Décision sur notre demande : associe l'appareil si acceptée. |

Le récepteur répond dans le même canal chiffré par des messages JSON (`protocol::Ack`) :
`{"ok":true,"offset":N}` juste après une offre de fichier (N = octets déjà reçus), puis, à la fin de
**chaque** connexion (texte compris), `{"ok":true}` ou `{"ok":false,"error":"…","code":"…"}`.
Codes : `not-paired` (le destinataire ne nous a pas associé : l'expéditeur l'explique et retentera),
`cancelled` (annulé par le destinataire : abandon définitif), `busy`, `io`, `error`.

### Transferts de fichiers

Tout est dans `src/transfer.rs`. Rien n'est chargé entièrement en mémoire : au plus un morceau de 256 Ko.

- **Envoi** : chaque couple fichier × destinataire devient une tâche `OutgoingJob` enregistrée dans
  `envois.json`. Le fil planificateur (toutes les 250 ms) lance chaque tâche dont le destinataire est
  joignable. L'empreinte SHA-256 est calculée une fois, puis gardée. Si le fichier source change
  (taille ou date), l'envoi est abandonné.
- **Coupure** (connexion perdue, appareil hors ligne, refus temporaire) : la tâche passe « En pause »
  avec la raison, puis est retentée (délai `Config::retry_delay`, 5 s, doublé à chaque échec, 60 s
  au plus) ou relancée dès que l'appareil revient en ligne. Elle survit au redémarrage de l'app.
- **Réception** : morceaux écrits dans `partiels/<id>.part` (dossier de données, caché) avec
  `partiels/<id>.json` (`IncomingMeta`). À la reprise, le récepteur tronque le partiel à un multiple
  de 256 Ko, recalcule l'empreinte de ce qui est déjà là et répond `offset` : l'expéditeur repart de
  ce morceau. Complet et vérifié → déplacé (renommage, ou copie si autre disque) dans le dossier de
  réception : réglage `save_folder`, sinon Téléchargements.
- **Délai de reprise** : `Settings::resume_hours` (48 h par défaut, de 1 h à 30 jours) depuis la
  dernière activité ; au-delà, tâches et fichiers partiels sont supprimés.
- **Annulation** (`Engine::cancel_transfer`) : côté expéditeur, envoie `file-cancel` (ou le fera au
  prochain contact) ; côté récepteur, supprime le partiel et refuse ensuite ce transfert (`cancelled`).
- Identifiants de transfert validés (`[A-Za-z0-9-]`, 64 max) car ils servent de nom de fichier ;
  noms reçus nettoyés (`util::sanitize_file_name`) ; trame de 4 Mo maximum ; connexion muette coupée
  après 30 s.

### Discussions — `src/chat.rs`

Conversations à deux et groupes, sans serveur : chaque auteur envoie lui-même ses messages à chaque
membre, par le même canal Noise que le reste.

- **Événements numérotés.** Tout ce qui change une conversation est un `ChatEvent` (`post`, `edit`,
  `delete`, `react`, `group-update`, `leave`) numéroté par son auteur : 1, 2, 3… sans trou
  (`seq`), avec une horloge de Lamport (`lamport`) et l'heure d'envoi (`sentMs`). Le destinataire
  n'applique l'événement `n` d'un auteur qu'après `n - 1` et répond (`Ack.offset`) par le dernier
  numéro appliqué sans trou. L'auteur renvoie tout ce qui n'est pas confirmé (par lots de 200
  événements ou 1 Mo, nouvel essai de 2 s à 30 s, et dès que le membre revient en ligne), même
  après un redémarrage ou des jours d'absence : **rien ne se perd, rien n'arrive dans le désordre**.
  Tous les membres trient l'affichage par (`lamport`, auteur) : même ordre partout.
- **Messages réseau** (`src/protocol.rs`) : `chat-events` (lot d'événements d'un auteur),
  `chat-typing` (« écrit… », éphémère, au plus toutes les 3 s, affiché 6 s), `chat-read` (tout est lu
  jusqu'à cette horloge de Lamport : « Lu », « Lu par … »).
- **Épingles** : événement `pin` {cible, épinglé ?}, par n'importe quel membre ; une ligne « X a
  épinglé « … » » apparaît dans le fil, et le bouton punaise de l'en-tête liste les messages épinglés.
- **Transfert** : « Transférer… » renvoie le texte et les pièces jointes disponibles dans une autre
  conversation ; le `post` porte `forwarded` (nom public de l'auteur d'origine).
- **Réactions** : n'importe quel émoji (grille de 48 ou saisie libre), 32 octets au plus.
- **Compatibilité** : un type d'événement inconnu (version plus récente) est compté puis ignoré
  (`ChatKind::Unknown`). Les versions d'avant la 0.9 ne connaissent pas `pin` : mettez tous les
  appareils à jour.
- **Menus** : menus d'actions dessinés par l'app (pas ceux du système) — Mac : `src/ui_mac_menus.rs`
  (bulles `NSPopover` avec icônes, rangée de réactions, grilles d'émojis et d'autocollants) ;
  Windows : `MenuRow`/`MenuPanel` et fenêtres surgissantes de `ui/chat.slint` (clic droit ou « ⋯ »).
- **Pièces jointes** : un message `post` décrit ses pièces (`id`, nom, taille, type `image`, `gif`,
  `video`, `sticker` ou `file`) ; les octets partent comme un transfert de fichier ordinaire
  (reprenable, vérifié) dont l'offre porte `chat: {conversation, attachment}`. Le récepteur les range
  dans `discussions/pieces/` et non dans Téléchargements. 15 pièces jointes au plus par message,
  1 Go chacune ; texte de 20 000 caractères au plus.
- **Groupes (« un membre présente »)**. Le créateur choisit des appareils qu'**il** a associés.
  Son événement `group-update` (nom + liste des membres avec leur clé publique) part par le canal
  authentifié : chaque membre fait confiance aux clés présentées, **pour ce groupe seulement**. Deux
  membres qui ne se sont jamais associés peuvent ainsi se parler dans le groupe ; un message n'est
  accepté que si la clé prouvée pendant la poignée de main Noise est celle d'un membre.
  **Depuis la 0.10, n'importe quel membre ajoute ses propres appareils associés** (`add-members`,
  liste des seuls nouveaux) ; le créateur garde `group-update` (compris par la 0.9). Les listes de
  membres ne font que **s'unir** (un événement plus ancien ne retire personne) ; un membre parti
  (`leave`) reste dehors sauf nouvel ajout explicite. Chaque envoi `chat-events` d'un groupe porte
  aussi `group` (nom, créateur, membres vus par l'expéditeur) : un membre ajouté plus tard rejoint la
  conversation dès le premier lot reçu d'un appareil qu'il a associé, et les membres qu'un appareil
  aurait manqués sont rattrapés. (Avant la 0.10, un membre ajouté après la création était refusé
  chez lui.) Seul le créateur renomme ; chacun peut quitter (lecture seule). Les nouveaux membres
  reçoivent l'historique.
- **Demandes d'ami** (`src/friends.rs`). Dans le menu du groupe, chaque membre non associé a
  « Ajouter en ami ». `friend-request {conversation}` part dès qu'il est en ligne ; il voit la demande
  dans sa page Appareils (Accepter / Refuser, empreinte affichée). Accepter associe les deux appareils
  **avec les clés présentées dans le groupe** (comme deux clics « Associer »), la réponse
  (`friend-answer`) étant renvoyée jusqu'à réception. Deux demandes croisées s'acceptent d'elles-mêmes ;
  une demande à un appareil déjà associé est acceptée aussitôt. Seule la réponse d'un appareil à qui
  l'on a demandé est acceptée hors groupe. État dans `amis.json` (demandes reçues gardées 30 jours).
- **Markdown** (`src/markdown.rs`, analyse par `pulldown-cmark`) : CommonMark complet + extensions
  GitHub (tableaux, cases à cocher, barré, encadrés `> [!NOTE]` / `[!TIP]` / `[!WARNING]`…) + habitudes
  de Discord (`__souligné__`, `-# petit texte`, retours à la ligne gardés tels quels) + couleurs
  `<font color="rouge">…</font>` ou `<span style="color:#e5484d">` (noms français ou anglais, `#rgb`,
  `#rrggbb`). `markdown::parse` rend une liste **plate** de blocs (texte, titre 1-3, petit texte,
  code, tableau, séparateur) avec leur puce, profondeur de liste et de citation ; chaque interface
  les empile (`src/ui_mac_markdown.rs`, `ui/markdown.slint` + `src/ui_slint/markdown.rs`). Blocs de
  code colorés par `src/highlight.rs` (Rust, JS/TS, C/C++/Java/Go/Swift…, Python, shell, SQL, Lua,
  JSON, YAML/TOML, HTML/XML, CSS ; thème One Dark sur fond sombre). Le HTML n'est jamais
  interprété (sauf couleur et `<u>`), seuls `http`, `https` et `mailto` deviennent des liens, et
  aucune image n'est chargée depuis Internet. Slint reçoit chaque bloc réécrit en Markdown strict
  (`spans_to_commonmark`, texte entièrement échappé) que son `StyledText` accepte toujours.
- **Autocollants** : une image devient un PNG de 320 px au plus (`image`), un GIF est gardé tel quel
  (animé) ; bibliothèque locale `discussions/autocollants/`, envoyée comme pièce jointe `sticker`.
- **Interfaces** : la liste des conversations à gauche, les messages groupés par auteur (avatar,
  nom, heure ; un nouveau bloc après 5 min), bulles, réponses citées, réactions, « (modifié) »,
  images et GIF animés dans le fil (Slint : images décodées et réduites par un fil de travail, GIF
  animés à 20 images/s seulement à l'écran), état « Envoi… / Distribué / Lu » sous son dernier
  message, « … écrit… » sous la zone de saisie. Une conversation affichée est marquée lue.
- **Limite** : pas de relais. Un message attend que son destinataire soit joignable **en même temps**
  que son auteur ; dans un groupe, chaque auteur livre lui-même ses messages à chaque membre.

### Canevas — `src/canvas.rs`

Pages partagées d'une conversation (pensées pour les groupes), que tous les membres écrivent
**ensemble et en direct**, en Markdown complet (titres, listes, cases, tableaux, couleurs, code…).

- **Fusion sans conflit** : chaque conversation a un document Yjs (crate `yrs`, CRDT) : `canevas`
  (carte id → titre, auteur, date en JSON), `texte/<id>` (texte), `versions/<id>` (historique).
  Une frappe ne transmet que la différence (préfixe et suffixe communs) ; deux personnes qui tapent
  en même temps, même hors ligne pendant des jours, obtiennent le même texte une fois réunies.
- **Synchronisation** : message `canvas-sync` {conversation, canevas en cours de modification,
  `update` = ce qui manque au destinataire d'après son dernier état connu, `state` = notre état} ;
  la réponse (`Ack.data`, JSON) contient ce qui nous manque et l'état du destinataire : un seul
  aller-retour. Chaque frappe part tout de suite vers les membres joignables (le planificateur
  tourne toutes les 150 ms) ; un échange complet a lieu au moins toutes les 30 s avec chacun.
  Seuls les membres (clé prouvée) d'une conversation non quittée sont acceptés.
- **Présence** : « X modifie… » pendant 5 s après chaque frappe reçue.
- **Historique intelligent** : les modifications d'une personne forment une version après 45 s de
  pause (ou 10 min d'écriture continue) : auteur, heure, caractères ajoutés/retirés, résumé (début
  de la partie modifiée) et texte complet. Ouvrir l'historique clôt la séance en cours. On peut
  voir une version, la **nommer** (jamais supprimée ensuite) ou la **restaurer** (devient une
  nouvelle version, l'état précédent reste). 150 versions au plus par canevas (les plus anciennes
  sans nom partent d'abord). Les versions sont partagées par tous les membres.
- **Éditeur** : texte Markdown à gauche, aperçu à droite (onglets « Modifier / Aperçu » si la
  fenêtre est étroite sous Windows), barre de mise en forme commune (`canvas::FORMATS`,
  `canvas::apply_format` : gras, italique, souligné, barré, titres, listes, cases, citation, code,
  bloc de code, tableau, séparateur, lien, couleurs). Quand le texte d'un autre arrive, le curseur
  reste au même endroit (`canvas::transform_offset`). Mac : `src/ui_mac_canvas.rs` (`NSTextView`) ;
  Windows : `ui/canvas.slint` + `src/ui_slint/canvas.rs` (`TextInput` dont on lit la position du
  curseur).
- Stockage : `canevas/<conversation>.yjs` (état complet, encodage Yjs v1), enregistré toutes les 2 s.

### Profils — `src/profile.rs`

Comme sur Discord : photo de profil (fixe ou GIF animé), bannière (image ou couleur), pronoms et
« À propos de moi » (Markdown, 190 caractères). Le nom affiché reste celui de l'appareil.

- **Réglages** : Paramètres → Mon profil (aperçu de la carte, photo, bannière, couleur, pronoms,
  présentation). Une image choisie est recadrée au centre et réduite (photo 256 × 256 en PNG,
  bannière 960 × 384 en JPEG). Une animation (GIF ou PNG animé) reste animée : réencodée en GIF
  (photo 160 × 160, bannière 600 × 240, moins d'images ou plus petite si besoin) pour tenir dans
  1,5 Mo (photo) ou 2,4 Mo (bannière) ; un GIF déjà léger est gardé tel quel (`src/animation.rs`).
  Windows : animée sur la carte de profil, et au survol d'un message (comme Discord).
- **Émojis personnalisés** (`src/emoji.rs`) : 60 au plus, carrés de 96 px (PNG, ou GIF s'ils sont
  animés, 512 Ko au plus). `ProfileInfo.emojis` en donne la liste (nom, id = 12 chiffres hexa du
  SHA-256, format, taille) ; les images, trop lourdes pour la trame du profil (4 Mo), sont demandées
  ensuite une à une par le contact (`emoji-fetch`, réponse en base64, servie seulement aux contacts),
  vérifiées puis rangées dans `profils/<id>/emojis/`. Dans un message ou une réaction, un émoji
  s'écrit `<:nom:id>` (comme Discord) ; la saisie montre `:nom:`, transformé à l'envoi
  (`encode_shortcodes`, nos émojis d'abord). Ailleurs (aperçus, Markdown complexe) : `:nom:`.
- **Diffusion** : message `profile` (en-tête `ProfileInfo` : version, textes, couleur, et pour chaque
  image son format, sa taille et son SHA-256 ; les octets des images suivent l'en-tête). Chaque
  appareil l'envoie lui-même à ses **contacts** : appareils associés et membres de ses groupes
  (joints grâce aux clés présentées par le créateur). Il retient la dernière version confirmée par
  chacun (`profil/envois.json`) et renvoie à chaque changement, dès que le contact est joignable
  (nouvel essai 30 s plus tard, 5 min après un refus).
- **Réception** : seulement d'un contact prouvé par sa clé ; images vérifiées (taille, format, SHA-256,
  décodables), rangées dans `profils/<id>/`. Une version plus ancienne est ignorée.
- **Affichage** : photo ronde dans les messages, la liste des discussions et l'en-tête ; clic sur
  une photo → carte de profil (bannière, photo, pronoms, présentation, « Discuter »).

### Émojis, GIF et thèmes

- **Sélecteur d'émojis** : tout le catalogue Unicode 15 (crate `emojis`), par catégories, avec
  recherche (noms anglais et codes courts), **favoris** (clic droit ou ☆ sous Windows, ⌥-clic sur
  Mac) et **récents**, gardés sur l'appareil (`emojis.json`), et les émojis personnalisés (les nôtres
  et ceux des contacts ; bouton + pour en ajouter). Le même sélecteur sert à réagir. Mac : bulle avec
  favoris, récents, personnalisés et courants ; le reste via le visualiseur de caractères de macOS.
- **Messages** : émojis seuls → en grand (images animées pour les personnalisés) ; texte simple avec
  émojis personnalisés → images dans la ligne (Windows) ; sinon `:nom:` dans le Markdown.
- **GIF de GIPHY** (`src/giphy.rs`) : bouton « GIF » de la zone de saisie ; tendances, recherche
  (`curl`, HTTPS, réponses et adresses de GIPHY seulement) et **favoris** (étoile sous Windows,
  ⌥-clic sur Mac ; `gif-favoris.json`, aperçus dans `giphy/apercus/`). Le GIF choisi est téléchargé
  (`giphy/envois/`) puis envoyé comme une pièce jointe : les contacts le reçoivent en direct. Clé
  d'API gratuite (developers.giphy.com) saisie dans la bulle (`settings.json` : `giphy_key`) ou
  variable `MAGICPLUS_GIPHY_KEY`.
- **Thèmes** (`src/themes.rs`) : Système, Clair, Sombre (couleurs Fluent) et 11 palettes (Graphite,
  Onyx, Nuit bleue, Améthyste, Forêt, Braise, Océan, Sakura, Menthe, Sable). Windows : Paramètres →
  Apparence (pastilles), palette complète via `Theme.custom`/`Theme.colors` (`ui/theme.slint`). Mac :
  apparence claire ou sombre et fond des fenêtres (`settings.json` : `theme`).
- **Présence** (`src/presence.rs`) : En ligne, Inactif, Ne pas déranger, Invisible (Windows : panneau
  « moi » en bas de la barre latérale ; Mac : Paramètres → Statut et notifications). Annoncée dans
  chaque `Hello` (`presence`) ; un invisible s'annonce `offline` : vu hors ligne, mais joignable (les
  messages passent). « En ligne » devient « Inactif » après N minutes sans clavier ni souris
  (`GetLastInputInfo` sous Windows, `CGEventSourceSecondsSinceLastEventType` sur Mac ; réglage
  `auto_idle_minutes`, 10 par défaut). Pastilles comme Discord : verte, lune jaune, rouge barrée, grise.
- **Préférences des notifications** (`settings.notifications`, `src/presence.rs`) : activées,
  discussions à deux, groupes (tous / mentions « @pseudo », « @nom », « @tous » / jamais), texte du
  message, son, même quand la discussion est affichée. Appliquées par `Inner::notify` à chaque `Notice`
  (genre `NoticeKind` : discussion à deux, groupe avec ou sans mention, autre) ; rien en « ne pas
  déranger ». L'affichage reste celui de l'arrière-plan (bulle de l'icône sous Windows, centre de
  notifications sur Mac). L'ancien réglage booléen de la 0.11 est encore lu.
- **Discussions façon Discord** : texte sans bulle sur toute la largeur ; une réponse montre au-dessus
  du message un trait courbe, la petite photo et le @nom de l'auteur cité, et le début de son message.

### Versions et mises à jour — `src/update.rs`, `.github/workflows/`

- **Publier** : une étiquette `vX.Y.Z` (annotée : son message devient les notes de version, affichées
  dans l'app) envoyée sur GitHub déclenche `versions.yml` : `MagicPlus-windows-x64.exe`
  (`windows-latest`), `MagicPlus-macos.zip` (`macos-latest`, exécutable universel Intel + Apple Silicon
  via `MAGICPLUS_UNIVERSAL=1 packaging/macos/build-app.sh`, macOS 11 minimum) et `SHA256SUMS.txt`.
  `verification.yml` lance clippy (Windows, macOS) et les tests (macOS) à chaque envoi sur `main`.
- **Chercher** : au démarrage (après 15 s) puis toutes les 6 h si `Settings::auto_update` (vrai par
  défaut), ou par « Rechercher maintenant » : `https://api.github.com/repos/Ywn509zz/magicplus/releases/latest`
  (60 requêtes/heure permises sans compte : largement assez). Sans Internet : échec silencieux.
- **Télécharger** : le fichier de la plateforme dans `mise-a-jour/` (dossier de données), puis
  vérification de la taille et du SHA-256 annoncés dans `SHA256SUMS.txt`. Tout passe par `curl`
  (fourni avec macOS et Windows 10/11, HTTPS seulement) : pas de bibliothèque TLS embarquée.
- **Installer** : Windows renomme l'exécutable en cours en `magicplus.old.exe` (permis même en cours
  d'exécution), met le nouveau à sa place et le lance. macOS remplace `MagicPlus.app` (l'ancienne est
  mise de côté en `.MagicPlus-ancienne-version.app`, à côté) et la relance avec `open`. L'interface
  quitte ; la nouvelle version efface les restes au démarrage. Une copie lancée hors de
  `MagicPlus.app` (`cargo run`) signale qu'elle ne peut pas se remplacer seule.
- **Sécurité** : la confiance repose sur HTTPS et sur le compte GitHub (pas de signature propre aux
  versions). Les autorisations macOS données à l'app (Accessibilité) peuvent être redemandées après
  une mise à jour, la signature locale changeant à chaque version.

### Presse-papiers et raccourcis

- Lecture et écriture du texte avec `arboard`.
- Raccourcis globaux (`global-hotkey`, `src/shared.rs`), enregistrés depuis le fil principal.
- « Copier et envoyer » simule Cmd/Ctrl+C dans l'application au premier plan (`osascript` sous macOS,
  PowerShell `SendKeys` sous Windows, `xdotool` sous Linux), après 350 ms pour laisser relâcher les touches.

### Arrière-plan, notifications, démarrage

- Le moteur tourne tant que le processus vit : la fenêtre se cache au lieu de se fermer.
  - **Mac** (`src/ui_mac_system.rs`) : `applicationShouldTerminateAfterLastWindowClosed` = non,
    clic sur le Dock = fenêtre, icône `NSStatusItem` (Ouvrir, Discussions, Quitter) avec le nombre de
    non-lus, pastille du Dock, et `NSProcessInfo.beginActivity` pour que « App Nap » ne ralentisse
    pas les annonces (sinon les autres appareils verraient celui-ci « hors ligne »).
  - **Windows** (`src/ui_slint/tray.rs`, API Win32) : icône de la zone de notification
    (`Shell_NotifyIcon`, menu Ouvrir / Quitter), boucle Slint `run_event_loop_until_quit`, première
    fermeture expliquée par une notification.
- **Notifications** : le moteur range un `Notice` (titre, texte en clair sans Markdown, conversation)
  pour chaque message reçu (trois au plus par lot, puis « N nouveaux messages ») et chaque demande
  d'ami ; l'interface les lit (`Engine::take_notices`) et ne montre que ce qui n'est pas déjà à
  l'écran. Mac : `UNUserNotificationCenter` (autorisation demandée au premier lancement ; hors d'un
  `.app`, en développement, `osascript`). Windows : bulles de l'icône, affichées comme des
  notifications Windows 10/11. Réglage `Settings::notifications`.
- **Démarrage** (`src/autostart.rs`, argument `--demarrage` = fenêtre cachée) : Mac
  `~/Library/LaunchAgents/com.magicplus.app.plist` (`open -g -a MagicPlus.app`), Windows
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\MagicPlus`, Linux
  `~/.config/autostart/magicplus.desktop`. L'état est lu dans le système, pas dans les réglages.

### Bluetooth — `src/bluetooth/`

- **Pourquoi le Bluetooth classique (RFCOMM)** : c'est le seul moyen radio commun à tous les
  ordinateurs visés, y compris les Mac de 2011 sans Bluetooth basse consommation ; il ne touche pas au
  Wi-Fi (contrairement à un point d'accès ou au Wi-Fi Direct) et le pare-feu de Windows ne le filtre
  pas. Portée d'une dizaine de mètres, débit d'environ 100 Ko/s.
- **Sécurité** : le lien Bluetooth n'est qu'un tuyau. Au-dessus passe exactement le même canal
  chiffré **Noise IK** qu'en TCP, avec les mêmes clés et la même association MagicPlus : un appareil
  Bluetooth inconnu ne peut rien envoyer. L'appairage du système sert seulement à autoriser la liaison
  radio.
- **Service** : chaque MagicPlus publie un service RFCOMM (UUID `4d61d1c2-8f3e-4b6a-9c51-2b7e0a9d3f11`)
  dans l'annuaire SDP. Mac : `IOBluetooth` sur un fil dédié qui fait tourner sa boucle d'exécution
  (`bluetooth/mac.rs`) ; Windows : Winsock `AF_BTH` + `WSASetService` (`bluetooth/windows.rs`).
- **Sondage** : toutes les 30 s, les ordinateurs appairés au système (classe « ordinateur ») qui ne
  sont pas déjà joignables par le réseau sont contactés ; premier octet `H`, échange des annonces
  `Hello` (u16 + JSON), fermeture. L'appareil est alors « en ligne via Bluetooth » 80 s. Un appareil
  sans MagicPlus est réessayé de plus en plus rarement (1 à 10 min).
- **Sessions** : premier octet `N`, puis le canal Noise. RFCOMM n'a pas de demi-fermeture : la fin
  d'envoi est une trame Noise de longueur nulle (jamais un vrai message). Une seule connexion à la fois
  vers un même appareil ; « en train d'écrire » ne passe pas par le Bluetooth.
- **Choix du chemin** (`Discovered::route`) : réseau local s'il a annoncé depuis moins de 8 s, sinon
  Bluetooth. Annonces et routes sont dans `engine.rs` (`Source`, `Route`), les liens dans `link.rs`.

### Stockage local

Dossier : `~/Library/Application Support/MagicPlus` (macOS), `%APPDATA%\MagicPlus` (Windows),
`~/.config/MagicPlus` (Linux). Remplaçable avec `MAGICPLUS_DATA_DIR`.

| Fichier | Contenu |
| --- | --- |
| `identity.json` | id, nom, plateforme, clé publique et **clé secrète** |
| `trusted.json` | appareils associés (id, nom, plateforme, clé publique) |
| `settings.json` | dossier de réception, durée de reprise, copie automatique du texte reçu, thème, clé GIPHY, adresses saisies à la main, recherche des mises à jour |
| `emojis.json`, `gif-favoris.json`, `giphy/` | émojis récents et favoris, GIF favoris, aperçus et GIF téléchargés de GIPHY |
| `mise-a-jour/` | version téléchargée en attente d'installation (effacée au démarrage suivant) |
| `profil/` | notre profil (`profil.json`, `avatar.*`, `banniere.*`, `emojis/`) et les versions confirmées par chaque contact (`envois.json`) |
| `profils/<id>/` | profils reçus des contacts (et leurs `emojis/`) |
| `canevas/<conversation>.yjs` | canevas partagés de la conversation (document Yjs) |
| `envois.json` | envois en attente ou à reprendre |
| `partiels/` | fichiers en cours de réception (`<id>.part`) et leur description (`<id>.json`) |
| `discussions/etat.json` | conversations : membres (et membres partis), compteurs envoyés/confirmés/appliqués, lectures |
| `amis.json` | demandes d'ami reçues, envoyées, et réponses à faire parvenir |
| `discussions/evenements/<conversation>.jsonl` | journal des événements, rejoué au démarrage |
| `discussions/pieces/`, `autocollants/`, `coller/` | pièces jointes reçues, bibliothèque d'autocollants, images collées |

---

## Arborescence

| Fichier | Rôle |
| --- | --- |
| `src/lib.rs` | Exporte le moteur (`Engine`, `Config`, `Snapshot` et les vues). |
| `src/engine.rs` | État partagé, association, presse-papiers, découverte, adresses manuelles, réception des trames. |
| `src/transfer.rs` | Transferts de fichiers reprenables : tâches d'envoi, planificateur, fichiers partiels. |
| `src/chat.rs` | Discussions et groupes : événements numérotés, synchronisation, lectures, autocollants. |
| `src/friends.rs` | Demandes d'ami entre membres d'un groupe : envoi, réponse, association. |
| `src/firewall.rs` | Pare-feu Windows (vérification, autorisation en un clic) et cartes réseau virtuelles. |
| `src/link.rs`, `src/bluetooth/` | Liens sous le canal chiffré (TCP, Bluetooth RFCOMM Mac/Windows), sondage des ordinateurs appairés. |
| `src/autostart.rs` | Lancement à l'ouverture de la session (agent macOS, clé `Run` Windows, `.desktop` Linux). |
| `src/ui_mac_system.rs`, `src/ui_slint/tray.rs` | Arrière-plan : icône (barre des menus, zone de notification), notifications du système. |
| `src/markdown.rs` | Markdown des messages (analyse, texte brut, réécriture pour Slint). |
| `src/update.rs` | Mises à jour : recherche sur GitHub, téléchargement vérifié, remplacement de l'app. |
| `src/profile.rs` | Profils : images, émojis personnalisés, diffusion aux contacts, réception. |
| `src/animation.rs` | Images animées (GIF, APNG) : décodage image par image, réencodage en GIF. |
| `src/emoji.rs` | Émojis : catalogue Unicode, jetons `<:nom:id>`, préparation des émojis personnalisés, favoris et récents. |
| `src/giphy.rs` | GIF de GIPHY : recherche, aperçus, envoi, favoris. |
| `src/presence.rs` | Présence (en ligne, inactif, ne pas déranger, invisible), inactivité du système, règles des notifications. |
| `src/canvas.rs` | Canevas partagés : document Yjs, synchronisation, versions, mise en forme. |
| `src/highlight.rs` | Coloration des blocs de code. |
| `src/ui_mac_markdown.rs`, `src/ui_mac_profile.rs`, `src/ui_mac_canvas.rs` | Mac : affichage du Markdown, profils (photos, carte, réglages), canevas. |
| `src/ui_slint/markdown.rs`, `profile.rs`, `images.rs`, `canvas.rs` + `ui/markdown.slint`, `ui/profile.slint`, `ui/canvas.slint` | Windows : Markdown, profils, images de profil, canevas. |
| `src/net.rs` | Annonces et écoute UDP, écoute TCP. |
| `src/secure.rs` | Canal chiffré Noise (poignée de main, messages chiffrés). |
| `src/protocol.rs` | Constantes (ports, tailles), `Hello`, `Message`, `Ack`. |
| `src/crypto.rs` | Paires de clés X25519, empreintes. |
| `src/store.rs` | Fichiers JSON (`StoredIdentity`, `TrustedPeer`, `Settings`), écriture atomique. |
| `src/util.rs` | Nom de la machine, tailles lisibles, noms de fichiers sûrs, destination unique. |
| `src/main.rs` | Démarre le moteur puis l'interface de la plateforme. |
| `src/shared.rs` | Commun aux interfaces : raccourcis globaux, ouvrir un fichier ou son dossier, heure locale, initiales. |
| `src/themes.rs` | Thèmes de couleur communs aux deux interfaces. |
| `src/ui_mac_gif.rs`, `src/ui_mac_menus.rs` | Mac : sélecteur de GIF ; menus, sélecteur d'émojis. |
| `src/ui_slint/emoji.rs` | Windows : lignes du sélecteur d'émojis, émojis personnalisés dans les messages. |
| `src/ui_mac.rs` + `src/ui_mac_chat.rs` | Interface macOS (AppKit via `objc2`) : barre latérale, 5 pages dont Discussions, alertes, menus. |
| `src/ui_slint.rs` + `src/ui_slint/chat.rs` | Interface Windows/Linux (Slint) : pages, puis la page Discussions (modèles, images, actions). |
| `ui/app.slint`, `ui/chat.slint`, `ui/theme.slint` | Fenêtre Slint (style Fluent), page Discussions, couleurs ; icônes dans `ui/icons`. |
| `tests/e2e.rs`, `tests/chat.rs` | Tests de bout en bout (transferts, discussions). |
| `packaging/macos/` | `build-app.sh` (fabrique `MagicPlus.app`), modèle `Info.plist`, icône `icon.png` (512 px). |
| `.github/workflows/` | `versions.yml` (publie une version à chaque étiquette `v*`), `verification.yml` (clippy et tests). |

---

## Variables d'environnement (développement)

| Variable | Effet |
| --- | --- |
| `MAGICPLUS_DATA_DIR=/chemin` | Autre dossier de données (seconde instance de test, captures). |
| `MAGICPLUS_GIPHY_KEY=clé` | Clé d'API GIPHY (sinon : celle saisie dans l'app). |
| `MAGICPLUS_PORTS=46454,46455` | Autres ports d'annonce et de connexion : une copie lancée à côté de la vraie, invisible des appareils du réseau. |
| `MAGICPLUS_FAKE_FIREWALL=1` | Affiche l'avertissement du pare-feu Windows (captures). |
| `MAGICPLUS_TEST_NOTICE=1` | Une notification d'essai 3 s après le démarrage. |
| `MAGICPLUS_START_PROFILE=moi` | macOS : ouvre le profil en grand (`moi` ou le nom d'un appareil). Slint : captures `profil-complet`, `nouveautes`. |
| `MAGICPLUS_NO_BLUETOOTH=1` | Pas de Bluetooth (copie de développement à côté de la vraie). |
| `MAGICPLUS_START_PAGE=0..4` | macOS : ouvre directement une page (0 Appareils, 1 Discussions, 2 Presse-papiers, 3 Transferts, 4 Paramètres). `MAGICPLUS_START_CHAT=n` ouvre la n-ième conversation, `MAGICPLUS_START_CANVAS=1` (ou `historique`) ses canevas. |
| `MAGICPLUS_SCREENSHOT_DIR=/dossier` | Slint : photographie chaque page (TGA) puis quitte. `MAGICPLUS_SCREENSHOT_PAGES=discussion,discussion-2` limite la série (dont `emojis`, `emojis-perso`, `emojis-categorie`, `gifs`, `gifs-favoris`, `parametres`). La première capture d'une série n'ouvre pas les fenêtres surgissantes : en mettre une autre avant. |
| `MAGICPLUS_DEMO_DIR=/dossier` | Avec `cargo test --test chat donnees_de_demo -- --ignored` : fabrique des discussions de démonstration. |
| `--features slint-on-mac` | Développement : compile l'interface Windows (Slint) sur un Mac, par exemple `cargo build --features slint-on-mac --target-dir target/slint-mac`. Lent sur un Mac sans Metal ; sert à vérifier et photographier l'interface. |
| `MAGICPLUS_DEBUG=1` | Windows/Linux : mesure les blocages de la boucle d'événements. |
| `SLINT_BACKEND=winit-software` | Windows/Linux : force le rendu logiciel de Slint. |
| `MAGICPLUS_FAKE_VERSION=0.4.0` | Fait croire à l'app qu'elle a cette version (essayer la recherche et l'installation d'une mise à jour). |
| `MAGICPLUS_UPDATE_REPO=compte/dépôt` | Autre dépôt GitHub où chercher les versions. |
| `MAGICPLUS_UPDATE_TEST=1` | Sans interface : cherche, télécharge, vérifie et installe la dernière version publiée, puis quitte (avec `MAGICPLUS_FAKE_VERSION=0.0.1`). |

Deux instances sur une même machine ne peuvent pas partager les ports 45454/45455 : pour tester à
deux, utiliser deux machines, `MAGICPLUS_PORTS`, ou les tests `cargo test` qui utilisent des ports dédiés.

---

## Limites connues et pistes

- Interface Windows essayée sur un vrai PC en version 0.2. Depuis (0.3, 0.4, discussions), elle est
  vérifiée par compilation (`cargo check --target x86_64-pc-windows-msvc`), par les tests du moteur et
  en la lançant sur le Mac (`slint-on-mac`), pas encore sur un PC.
- Le protocole est `magicplus/4` : un Mac et un PC doivent avoir **la même version** pour se voir.
- `.app` macOS signée seulement localement (pas de notarisation Apple) ; pas d'installateur Windows
  (l'`.exe` est autonome).
- Une connexion par fichier et par destinataire ; les fichiers d'un même lot partent en parallèle.
- Discussions sans relais : un message attend que son destinataire et son auteur soient en ligne en
  même temps. Pas de recherche dans l'historique ; 300 derniers messages affichés par conversation.
- Bluetooth : il faut appairer les ordinateurs une fois dans les réglages du système ; pas de
  Bluetooth sous Linux pour l'instant. Liaison radio vérifiée sur Mac (service publié), pas encore
  entre deux vraies machines au moment de la 0.12.
- Texte seulement pour le presse-papiers (pas d'images).
- Si le réseau isole complètement les appareils entre eux (même en unicast), aucune connexion directe
  n'est possible : il faudrait un relais, contraire au principe « sans serveur ».
