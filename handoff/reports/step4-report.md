# Étape 4 : serveur et web UI

> **Mise à jour après revue.** La branche est maintenant en fast-forward sur `4820d9d`, et les corrections de revue sont appliquées. Voir la section « Review fixes » en fin de rapport, qui **prévaut** sur les sections précédentes quand elles divergent : écarts 3, 5, 6 et 7, statut du lien symbolique, 400 de `?since=1h`, et vérification finale.

Le travail est dans le worktree `/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step4`, branche `feat/visibility-0.5-step4`, basée sur `8d2f5ff`. Rien n'est commité. Le checkout principal n'a pas été modifié, seul ce rapport y a été écrit. La couche de lecture (`vibe-pipeline`) n'a pas bougé : aucun accesseur ajouté.

## Fichiers modifiés

- `crates/vibe-cli/src/server/api/read.rs` (nouveau, 420 lignes) : les six nouvelles routes.
  - Le module est déclaré par `mod read;` dans `api.rs`, pour ne pas toucher `server/mod.rs`, que l'étape 3 a modifié.
- `crates/vibe-cli/src/server/api.rs` (+8) : `mod read;` et six `.route(...)`. Les routes existantes, l'authentification et le contrôle du `Host` sont inchangés.
- `crates/vibe-cli/src/server/web/index.html` : les vues Activity et History, l'onglet Trace, le flux global unique. La page reste un seul fichier (voir l'écart 1).
- `crates/vibe-cli/tests/serve.rs` :
  - `start_with` est découpé en `new_project()` et `serve_in(dir, args)`, sans changer son comportement, pour pouvoir lancer un serveur en `--script` (`--script` exclut `--provider`) ;
  - 4 tests ajoutés, avec leurs helpers.
- `docs/src/user/cli.md`, section `vibe serve` : paragraphe sur les vues, 6 lignes dans la table de l'API, paragraphe sur le curseur, les filtres, les en-têtes et le SSE, phrase sur le confinement des traces.

**Conflit prévisible avec `4820d9d` (étape 3, commitée entre-temps).** Elle modifie aussi `tests/serve.rs` (test ajouté en fin de fichier), `server/mod.rs` (je n'y ai pas touché) et `cli.md` (d'autres sections que `vibe serve`, plus le synopsis de `serve`). J'ai demandé au team-lead l'autorisation d'avancer ma branche en fast-forward sur `4820d9d`, sans réponse ; je suis resté sur `8d2f5ff`.
- Dans `serve.rs`, mes modifications sont un découpage de `start_with` et des ajouts en fin de fichier : il faudra garder les deux blocs de fin.
- Dans `cli.md`, il faudra peut-être résoudre le paragraphe d'introduction de `vibe serve` à la main.

## Référence des routes (telle que documentée)

| Méthode et chemin | Effet |
|---|---|
| `GET /api/events?after=CURSOR&since=TIME&type=T&task=REF&limit=N` | événements journalisés de toutes les tâches, en `TaggedEnvelope`, du plus ancien au plus récent ; les `N` derniers après filtrage (1000 par défaut, 10000 au plus) |
| `GET /api/stream?after=CURSOR&since=TIME&type=T&task=REF` | SSE globale : événements après le point de départ, puis les nouveaux (y compris ceux des tâches créées ensuite), plus le texte streamé des runs lancés par ce serveur |
| `GET /api/history?all=bool` | `Vec<TaskHistory>` (= `vibe history --json`) |
| `GET /api/history/{ref}` | `TaskHistory` de n'importe quelle tâche ; 404 si la tâche est inconnue |
| `GET /api/tasks/{ref}/trace?run=RUN&all=bool` | `RunTrace` du dernier run, du run dont l'id commence par `RUN`, ou `Vec<RunTrace>` avec `all` ; 404 si la tâche n'a jamais tourné ou si aucun run ne correspond ; 400 si le préfixe est ambigu ou si `run` et `all` sont donnés ensemble |
| `GET /api/tasks/{ref}/trace/{call}/output` | sortie complète en `text/plain; charset=utf-8` ; 400 si l'id n'a pas 12 chiffres hexa (ou est nul) ; 404 si l'appel est inconnu ou si sa sortie n'est pas tracée ou a disparu ; 500 si `read_output` refuse (lien qui sort de `.vibe/tool-output`) |

**Règles communes à `/api/events` et `/api/stream`**

- **Filtres.** `type` et `task` sont répétables (`Query<Vec<(String, String)>>`, sans nouvelle dépendance). Un type inconnu donne 400, une tâche inconnue 404.
- **Point de départ.** `Last-Event-ID` (stream seulement) passe avant `after`, qui passe avant `since`.
  - Un curseur ou une date RFC 3339 invalide donne 400.
  - Sans aucun des trois, le stream part de maintenant (`AllEventsFollower::from_end`) et `/api/events` renvoie tout.
- **En-têtes de `/api/events`.**
  - `X-Vibe-Read-Errors: <n>` : nombre de journaux illisibles. Chaque erreur est aussi journalisée (`tracing::warn`).
  - `X-Vibe-Cursor` : curseur du dernier événement lu **avant** filtrage, pour reprendre avec `after`. C'est un ajout, voir l'écart 2.
- **Trames SSE.** `event` = nom du type, `data` = `TaggedEnvelope` JSON, `id` = `EventCursor` compact (`<nanos>-<number>-<seq>`).
  - `agent_delta` n'a pas d'`id` et n'est pas rejoué.
  - Un commentaire de keep-alive part toutes les 15 s.
- **Boucle du stream.**
  - `poll` toutes les 300 ms. Les erreurs par tâche sont journalisées une fois par tâche.
  - Entre deux `poll`, le bus du `RunManager` est écouté. Un `agent_delta` est étiqueté avec sa tâche via une table run → (tâche, numéro), construite à partir des événements lus.
  - Pour un run déjà en cours à la connexion (son `run_started` précède le point de départ), `manager.active()` et `load_run_state` servent de repli, au plus une fois par tick.
  - `Lagged` : on continue. `Closed` : on arrête d'écouter le bus, sans boucle active.
- **Configuration de l'historique.** Elle vient de `app::load_config(root)`, comme `vibe history`, et non de `ctx.config`, qui porte les overrides de `serve` (`--workspace`).
  - `task_branch` est réimplémenté en privé dans `read.rs` : `task.branch`, sinon `worktree_location` si le workspace utilise des worktrees. C'est une copie de celui de l'étape 3, à fusionner après rebase (voir l'écart 3).

## Web UI

- **Barre de vues** : Tasks, Activity, History, Evaluations. Le style, le JS vanilla et la CSP de la page sont inchangés.
- **Flux unique.**
  - Au chargement : `GET /api/events?limit=500`, qui remplit le fil Activity et donne `X-Vibe-Cursor`.
  - Puis un seul `EventSource` sur `/api/stream?after=<curseur ou 0-0-0>&token=…`. Les écouteurs sont enregistrés pour chaque type à partir d'une constante `GROUPS`, qui sert aussi aux filtres.
  - Le navigateur se reconnecte seul avec `Last-Event-ID`, que le serveur fait passer avant l'`after` périmé de l'URL. Si le flux est fermé (serveur redémarré), il est rouvert 5 s plus tard depuis le dernier curseur reçu.
  - Ce flux alimente : le fil Activity ; le journal de la tâche sélectionnée ; le texte live (`agent_delta` de la tâche sélectionnée) ; les jauges de budget (`budget_updated`) ; le rafraîchissement du tableau et du détail (sur `run_started`, `artefact_written`, `subtask_updated`, `approval_*`, `run_finished`, `paused`, `merged`, avec un regroupement de 300 ms pour la liste) ; l'historique, rechargé sur `run_finished` si la vue est ouverte.
  - Polling : la liste des tâches toutes les 10 s, car la création d'une tâche n'est pas un événement ; toutes les 2 s tant que le flux n'est pas ouvert ; le détail toutes les 3 s seulement si le flux est coupé.
- **Tâche sélectionnée.**
  - Le journal est rempli par `GET /api/tasks/{id}/events` (dernier run, route inchangée), puis par le flux global.
  - Il est dédupliqué par `(run, seq)` et trié par `seq` après le chargement, pour couvrir la course entre les deux sources.
  - Un `run_started` d'un autre run repart d'un journal vide. Plafond : 2000 lignes, comme le fil Activity.
- **Activity.**
  - 7 puces à bascule (`aria-pressed`) : agents, tools, phases, git, approvals, budget, logs. Les 22 types y sont tous rangés.
  - La barre d'outils est construite une seule fois. Les mises à jour ne touchent que les libellés, et les options du sélecteur ne sont reconstruites que si la liste des tâches change et que le sélecteur n'a pas le focus. Ainsi, un clic ou une liste ouverte n'est pas perdu pendant un run.
  - Un sélecteur de tâche, un bouton Pause/Resume (le compteur « N new » gèle le rendu), un indicateur live/connecting.
  - Chaque ligne affiche heure, `#N` et description. Un clic ou Entrée ouvre la tâche dans la vue Tasks.
  - `describe()` couvre maintenant `committed`, `merged`, `budget_updated` et un `tool_returned` réussi.
- **History.**
  - Table `/api/history` : #, title, status, runs, commits, files (`~` si approximatif), tokens (`+` si borne inférieure), active, cost (`–` sans tarif), finished. Case « Show failed/cancelled », bouton Reload, légende.
  - Une ligne (clic ou Entrée) se déplie et charge `/api/history/{id}` : runs (état, statut, reprises, temps actif, tokens, phases ✓/✗/…, merge, approbation en attente, dernière erreur), commits (sha, message, fichiers), fichiers modifiés (lettre A/M/D/R/C/?, badge de source, badge « approximate »), validations du dernier run qui en a, dernier QA, problèmes.
- **Onglet Trace** (détail de tâche).
  - Charge `/api/tasks/{id}/trace?all=true`. Un sélecteur de run apparaît quand il y en a plusieurs ; le dernier est choisi par défaut ; bouton Reload.
  - Chaque appel affiche : #, rôle, sous-tâche (titre du plan), outil, durée, statut (ok / exit N / error / timed out / no result), badge « paired by order », call id.
  - Déplié (clic ou Entrée/Espace, `aria-expanded`) : arguments en JSON indenté, aperçu, et bouton « Load complete output » qui appelle `apiText()` vers `/trace/{call}/output` et affiche la réponse dans un `pre` monospace. Sans fichier tracé, une mention l'explique.
  - Pied : nombre d'appels, nombre d'erreurs, fichiers écrits.

Contrôle de la page : `node --check` sur le script, et le test `the_page_has_every_view_and_listens_to_every_event_type`. **Pas de session dans un vrai navigateur** : le rendu n'a pas été vérifié à l'œil.

## Écarts et raisons

1. **Page restée en un seul fichier.** La CSP de la page (`script-src 'unsafe-inline'`, sans `'self'`) bloquerait un `app.js` séparé. Il faudrait aussi une route d'asset non authentifiée. Aucun bénéfice ici.
2. **En-tête `X-Vibe-Cursor` ajouté à `GET /api/events`.** Un client JS ne peut pas recalculer le curseur en nanosecondes à partir de `at`. Sans cet en-tête, il n'a aucun moyen fiable de reprendre le flux après le chargement initial.
   - Il vaut le dernier événement lu avant filtrage : une requête filtrée qui ne renvoie rien donne quand même un point de reprise.
   - Il est documenté.
3. **`task_branch` dupliqué** dans `read.rs`, car ma base (`8d2f5ff`) n'a pas `commands/history.rs`. Après rebase sur `4820d9d`, on peut le rendre `pub(crate)` dans `commands/history.rs` et supprimer la copie.
4. **`/api/history` utilise `load_config(root)`** et non `ctx.config`, pour donner exactement le même résultat que `vibe history`. Sinon, `serve --workspace in_place` changerait la dérivation des branches.
5. **Trace.**
   - `run` accepte un id complet ou un préfixe unique, comme `vibe trace --run`.
   - Avec `all` et aucun run : 404, lecture littérale de « 404 when no run », là où la CLI renvoie `[]`.
6. **Refus d'un lien symbolique sortant : 500.** C'est la conversion existante de `vibe_core::Error` (erreur de stockage), sans corps qui divulgue le contenu. Un chemin enregistré hors du store (`..`) est écarté par la couche de lecture : 404.
7. **`since` accepte seulement RFC 3339**, sans âge `1h` comme la CLI. `?since=1h` donne 400, et c'est testé.
8. **Stream sans point de départ = à partir de maintenant.** La page passe toujours un `after` (le curseur de l'en-tête, ou `0-0-0` si le projet est vide).

## Tests ajoutés (`tests/serve.rs`, 4 tests)

- **`the_page_has_every_view_and_listens_to_every_event_type`** : `GET /` contient les 4 boutons de vue, l'onglet Trace et `/api/stream?`. Chaque entrée de `vibe_core::Event::TYPES` doit figurer dans la constante `GROUPS`, qui produit les écouteurs SSE et les filtres.
  - Contrôle par mutation : sans `"merged"` dans `GROUPS`, le test échoue (`GROUPS lacks merged`), puis passe une fois le code restauré.
- **`global_events_are_filtered_and_resumable`** :
  - `X-Vibe-Read-Errors: 0` ; `X-Vibe-Cursor` égal au curseur du dernier événement, et il se relit comme `EventCursor` ;
  - curseurs strictement croissants ;
  - `type`, `type` répété avec `task`, `task=2` (vide), `limit=3` (= les 3 derniers), `after=<curseur du 3e>` (= la suite exacte), `since` futur et passé ;
  - 400 pour `type`, `after`, `since` et `limit` invalides ; 404 pour `task=99` ;
  - la route par tâche garde sa forme (pas de champ `task`) ;
  - `events.jsonl` de la tâche 2 remplacé par un répertoire : 200, `X-Vibe-Read-Errors: 1`, et les événements de la tâche 1 sont intacts.
- **`the_global_stream_replays_then_follows_new_tasks`** :
  - rejeu depuis `after=<curseur du 3e>` : le premier `id` est celui du 4e ;
  - une tâche 2 est créée et lancée pendant la connexion, et arrive jusqu'à son `run_finished` ;
  - chaque `id` se relit comme `EventCursor`, égal à `tagged.cursor()` et strictement croissant ; seules les trames `agent_delta` peuvent ne pas avoir d'`id` ; `event` égale le type de `data` ;
  - le mock streame son texte mot par mot : au moins une trame `agent_delta` sans `id`, étiquetée `"number":2`. Le chemin d'étiquetage des deltas est donc couvert de bout en bout, table run → tâche ou repli `learn_active_runs` selon le moment où arrive le premier poll ;
  - filtres `type`/`task` sur le stream ;
  - `Last-Event-ID` gagne contre `after=0-0-0` ;
  - 400 pour un `after` invalide, 401 sans jeton.
  - **Contrôle par mutation** : en inversant la priorité (`after` avant `Last-Event-ID`), ce test échoue (`serve.rs:506`), puis passe une fois le code restauré.
- **`history_and_trace_of_a_scripted_run`** (serveur en `--script`, `write_file` dans `hello.txt`) :
  - `/api/history` contient seulement #1 (#2 est en backlog), `state` finished, `commits ≥ 1`, `hello.txt` ; `?all=true` ;
  - `/api/history/1` se désérialise en `vibe_pipeline::TaskHistory` avec des commits ; `/history/2` a `runs: []` ; `/history/99` donne 404 ;
  - trace : `RunTrace` avec `files_written == ["hello.txt"]` ; `?run=<8 caractères>` donne la même trace ; `?all=true` donne une liste d'une trace ; `?run=zzzz`, la tâche 2 et la tâche 99 donnent 404 ;
  - sortie : 200, `text/plain`, et le texte contient `hello.txt` ;
  - 404 pour un id inconnu ; 400 pour `xyz`, `000000000000`, `..%2F..%2FREADME.md` et 13 chiffres ;
  - évasion de chemin, cas 1 : un `tool_returned` forgé avec `output_file: ".vibe/tool-output/../../<secret>"` et le call id `abcdefabcdef`.
    - Il apparaît dans `/api/tasks/1/trace`, apparié, avec `output_file: null` : la couche de lecture a reçu le chemin et l'a écarté.
    - Sa sortie donne 404 avec le message « not traced », sans le secret.
  - évasion de chemin, cas 2 (unix) : le fichier de trace réel remplacé par un symlink vers le secret donne une réponse non-200, sans le secret.
  - La suite `serve` a été relancée 5 fois de suite : 7/7 à chaque fois.
- Les 3 tests existants (`api_requires…`, `create_run_and_follow_a_task`, `evaluation_summaries_are_listed`) passent sans modification.

## Smoke manuel

Projet temporaire. Tâche 1 lancée avec `vibe run 1 --provider mock --workspace in_place`, tâche 2 avec `--script` (`write_file`). Puis `vibe --json serve --port 0 --provider mock` en arrière-plan.

```
$ GET /api/events?type=run_started&type=run_finished (headers + body)
HTTP/1.1 200 OK
content-type: application/json
x-vibe-read-errors: 0
x-vibe-cursor: 1790542625667433000-1-31
[{"task":"00c3fff5-…","number":1,"schema":2,"seq":1,"at":"2026-09-27T20:57:05.614504Z","event":{"type":"run_started",…}},{…"seq":31,…"event":{"type":"run_finished",…

$ GET /api/history
1 task(s): {'number': 1, 'totals': {'usage': {'input_tokens': 2695, 'output_tokens': 165, …}, 'active_ms': 53, 'runs': 1, 'commits': 0, 'complete': True}, 'validations_passed': None, 'cost': None, 'errors': []} runs [('finished', 'ready')] files {'source': {'kind': 'none'}, 'approximate': False, 'files': []}
$ GET /api/history/1
200
$ GET /api/history/9
{"error":"no task matches `9`"}
$ GET /api/tasks/1/trace
{"run":"081bc277-…","calls":[],"files_written":[]}
$ GET /api/tasks/1/trace/xyz/output
{"error":"invalid call id `xyz` (12 hex digits expected)"} 400
$ GET /api/tasks/1/trace/0123456789ab/output
{"error":"no call `0123456789ab` in this task"} 404
$ GET /api/stream?after=0-0-0&type=run_finished (3 s)
event: run_finished
data: {"task":"00c3fff5-…","number":1,"schema":2,"seq":31,"at":"2026-09-27T20:57:05.667433Z","event":{"type":"run_finished",…
id: 1790542625667433000-1-31

$ GET / | grep views
["trace", "Trace"]
/api/stream?
id="view-activity"
id="view-evals"
id="view-history"
id="view-tasks"
server stopped

(second server, after the scripted run of task 2)
$ GET /api/tasks/2/trace (trimmed)
files_written ['hello.txt']
{'call': 'c758228e4504', 'role': 'coder', 'tool': 'write_file', 'input': {'content': 'Hello, world!\n', 'path': 'hello.txt'}, 'duration_ms': 0, 'is_error': False, 'paired': 'id', 'output_chars': 31}
$ GET /api/tasks/2/trace/c758228e4504/output
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8

Created `hello.txt` (14 bytes).

$ GET /api/history (trimmed)
2 Write the greeting module commits 1 files ['hello.txt'] commits
1 Fix typo in README commits 0 files [] none
server stopped
```

Les deux serveurs ont été arrêtés par SIGINT ; `pgrep` ne trouve plus de processus.

## Vérification (worktree step4)

- `cargo fmt --all -- --check` : `fmt exit 0`, aucune sortie.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` (via `rtk proxy`, après `touch`) : exit 0, 0 ligne `warning` ou `error`. Un premier passage signalait `cloned_ref_to_slice_refs` dans le test, corrigé par `std::slice::from_ref`. Dernières lignes :
  ```
      Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step4/crates/vibe-cli)
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s
  ```
- `cargo test --workspace --all-features` (via `rtk proxy`) : `test exit 0`, 28 lignes `test result`, **630 passés, 0 échec**, soit 626 à `8d2f5ff` plus 4. Suite `serve` : 7 passés. Dernière ligne :
  ```
  test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  ```

## Review fixes

**Intégration.** Dans le worktree, dans cet ordre :
1. `git stash -u` ;
2. `git merge --ff-only 4820d9d` : la branche `feat/visibility-0.5-step4` est désormais à `4820d9d` ;
3. `git stash pop`.

Deux fichiers ont posé problème :
- `cli.md` a fusionné sans conflit ;
- `tests/serve.rs` avait un conflit : le test de l'étape 3 et mes tests étaient ajoutés au même endroit en fin de fichier. J'ai gardé les deux et rétabli l'accolade fermante.

Le stash a été supprimé une fois le conflit résolu. `task_branch` (`commands/history.rs`) est passé en `pub(crate)` et sa copie dans `read.rs` est supprimée.

**Fichiers touchés en plus de ceux de la première passe :**
- `commands/history.rs` : `pub(crate)` ;
- `commands/trace.rs` : ajout de `runs_of`, `PrefixMatch` et `find_prefix` en `pub(crate)`, `match_prefix` passe par eux, sans changement de comportement de la CLI ;
- `server/mod.rs` : +1 ligne, le champ `streams`.

### Majeur
1. **Symlink hors de `.vibe/tool-output`.**
   - Correction : toute erreur de `read_output` donne maintenant un 404 avec le message générique « the output of this call was not traced, or was discarded ». Le détail part dans `tracing::warn` et le corps ne contient aucun chemin.
   - Test : `history_and_trace_of_a_scripted_run` vérifie le statut 404, l'absence de `TOP SECRET` et l'absence de `tool-output` dans le corps.

### Mineurs
2. **Un seul lecteur partagé** (`StreamHub` dans `read.rs`, `Inner.streams: Arc<StreamHub>`).
   - Fonctionnement :
     - Une tâche tokio, lancée par le premier stream, poll les journaux toutes les 300 ms, écoute le bus du `RunManager` et publie sur un `broadcast` de 1024 éléments.
     - Elle s'arrête quand plus aucun stream n'écoute. Le verrou `running` est pris en même temps que l'abonnement, ce qui évite qu'elle s'arrête pendant qu'un client arrive.
     - Elle ne garde qu'un `Arc<FileTaskStore>` et un `RunManager`, pas `Inner` : l'`Arc::try_unwrap(ctx)` de l'arrêt reste possible.
   - Côté client :
     - Le client s'abonne **avant** son rejeu, puis rejoue depuis son point de départ avec `AllEventsFollower::new(after).poll()`.
     - J'ai préféré le follower à `all_events`, qui échoue au premier journal illisible, alors que le follower renvoie les erreurs tâche par tâche.
     - La jonction entre rejeu et direct est dédupliquée par tâche avec le dernier curseur envoyé.
     - En cas de `Lagged`, le client rejoue depuis son dernier curseur.
     - La fermeture du client est détectée par `tx.closed()`, ce qui libère la place et l'abonnement.
   - Plafonds :
     - 32 streams simultanés, 503 au-delà ;
     - `/output` coupé à 8 MiB sur une frontière de caractère, avec la marque « … (output cut: N more bytes) » et l'en-tête `X-Vibe-Truncated: true`.
   - Tests :
     - `many_clients_share_the_stream_and_a_replaced_log_is_not_sent_twice` : 3 clients simultanés reçoivent chacun tout le rejeu, puis le run d'une tâche créée pendant la connexion ; au-delà de 32 streams, 503 ;
     - découpe à 8 MiB dans `history_and_trace_of_a_scripted_run` : fichier de 9 MiB, en-tête présent, fin « 1048576 more bytes » ;
     - test unitaire `outputs_are_cut_on_a_character_boundary`, UTF-8 multi-octets.
   - Limite restante : un rejeu depuis `after=0-0-0` charge encore tout le journal en mémoire pour ce client, une seule fois. La page passe toujours le curseur de `X-Vibe-Cursor`.
3. **Journal tronqué ou remplacé.**
   - Le lecteur partagé garde le dernier curseur publié par tâche et ne republie rien d'égal ou d'inférieur. Le client applique la même règle.
   - J'ai utilisé le curseur plutôt que `(number, seq)` : `seq` repart à 1 à chaque run, alors que le curseur est monotone par tâche.
   - Test : dans le même test, `events.jsonl` de #1 est remplacé par une copie (nouvel inode), et aucune trame de #1 n'est renvoyée ensuite.
   - Contrôle par mutation : en désactivant les deux filtres (lecteur et client), le test échoue (« sent again »), puis passe une fois le code restauré.
4. **`?token=` limité aux deux streams.**
   - Correction : `is_stream_path` n'accepte que `/api/stream` et `/api/tasks/{x}/stream`, avec ou sans le préfixe `/api` que le routeur imbriqué retire.
   - Tests :
     - test unitaire `only_event_streams_take_the_token_from_the_query` ;
     - `new_routes_need_the_token_in_the_header` : 401 sans jeton sur chacune des 6 nouvelles routes ; 401 avec `?token=` sur `/api/events`, `/api/history`, `/api/history/1`, `/trace` et `/output` ; 401 sur `/api/history/stream?token=`.
5. **Préfixe de run.**
   - Correction : `find_prefix` de `commands/trace.rs` (espaces retirés, minuscules) est partagé avec la CLI, et `runs_of` remplace les deux collectes dupliquées.
   - Codes : 404 si aucun run ne correspond, 400 si plusieurs correspondent.
6. **`since` accepte aussi un âge.**
   - Correction : `since` passe par `crate::cli::parse_since`, donc `30m`, `2h` et `1d` en plus de RFC 3339.
   - Test : `?since=1h` renvoie tout, `?since=yesterday` donne 400.
7. **Référence de tâche ambiguë → 400.**
   - Correction : dans `api::find`, donc pour toutes les routes, y compris les anciennes, qui renvoyaient 500. La détection repose sur le message « is ambiguous » de `find_by_prefix`, qui ne fournit pas de type d'erreur distinct ; c'est commenté dans le code.
   - Test : `an_ambiguous_task_reference_is_a_bad_request` crée des tâches jusqu'à ce que deux ids partagent une première lettre a-f ou `0`, qu'aucun numéro ne masque, puis obtient 400 sur `/api/events?task=`, `/api/history/` et `/api/tasks/`.
8. **Web, événement de forme inattendue.** `line()` enveloppe `describe()` dans un try/catch et affiche le type brut. Elle sert au fil Activity et au journal de la tâche.
9. **Web, défilement du fil.** Le fil ne défile plus que si la vue était déjà en bas, et plus du tout en pause, car le rendu est gelé.
10. **Web, reconstruction du détail.**
    - Sur les onglets Trace et Changes, le détail n'est reconstruit que sur `run_started`, `approval_requested`, `approval_resolved`, `run_finished` et `paused`.
    - Sur les autres onglets, le comportement est inchangé.
    - Le texte live ne redessine que l'onglet Activity.
11. **Web, mémoire et rafraîchissements.**
    - Un nouveau run vide `seen` (en gardant la clé courante), `outputs` et `open`.
    - `state.live` est borné à 8000 caractères, ramenés aux 4000 derniers.
    - `refreshDetail` passe par `refreshTasksSoon`, avec un regroupement de 300 ms.
    - `loadHistory` n'est relancé que sur un `run_finished` récent (`at` ≥ ouverture du flux − 5 s), pas sur les rejoués.
    - **Bug trouvé au passage et corrigé :** après `select()`, le journal chargé par `/api/tasks/{id}/events` n'était pas redessiné. L'onglet Activity d'une tâche terminée restait vide jusqu'au prochain événement.
12. **Ordre des curseurs entre processus.** Documenté dans `cli.md` :
    - un stream ouvert envoie l'événement une fois, donc les ids ne sont pas toujours croissants ;
    - un client déconnecté qui reprend à son dernier id ne reçoit pas un événement horodaté avant cet id ;
    - la parade est de recharger `/api/tasks/{ref}/events`.

    Je n'ai pas touché à `persistence.md`.
13. **Docs (`cli.md`).**
    - `?token=` réservé aux deux streams.
    - Codes d'erreur : 400 pour une référence ambiguë, un type, un curseur, une date ou une limite invalides ; 503 pour trop de streams. Plus aucun 500 annoncé.
    - Ligne trace : 404 quand la tâche n'a pas tourné, à la différence de la CLI (`null` / `[]`) ; 400 pour un préfixe ambigu ou `run` avec `all`.
    - Ligne output : 8 MiB et `X-Vibe-Truncated`.
    - `since` avec des âges ; lecteur partagé et plafond de 32 ; lien symbolique → 404.

**Flake préexistant corrigé.** `create_run_and_follow_a_task` (test antérieur à l'étape 4) a échoué une fois sur `GET /api/tasks/99 → 200`. `99` est aussi un préfixe d'id hexadécimal : un id qui commence par `99` le résout, soit un cas sur 256. Cette référence et les miennes (`task=99`, `/history/99`, `/tasks/99/trace`) utilisent maintenant `zzz`, qui ne peut être ni un numéro, ni un répertoire, ni un préfixe hexa. La suite `serve` a ensuite passé 10 fois de suite, 11/11 à chaque fois.

**Smoke de l'UI dans un vrai navigateur** (Chrome headless piloté par CDP, script Node, serveur réel, projet avec une tâche mock et une tâche scriptée, puis une troisième créée depuis la page) :
```
summary: 2 task(s), 0 running
activity chips: agents=true tools=true phases=true git=true approvals=true budget=true logs=true Pause=
activity lines: 68        (tools off: 66)
history rows: 2 ; Write the greeting module ; ready ; 1 ; 1 ; 1~ ; 4.4k ; 45 ms ; – ; 27/09/2026 22:57:34
              1 ; Fix typo in README ; ready ; 1 ; 0 ; 0 ; 2.9k ; 53 ms ; – ; 27/09/2026 22:57:05
history detail: Runs (1)ef3b22b5 finished → ready … assess ✓ plan ✓ build ✓ qa ✓ merge ✓ Commits (1)93c3af4e vibe: complete subtask 1 - Write hello.txt … Files (1) from commit events approximate ? hello.txt Last QA: round 1, approved
click on a "#2" Activity line → view: board, detail title: 002 Write the greeting module ready
trace: Reload #1 coder · Write hello.txt write_file 0 ms ok c758228e4504 1 call(s), 0 error(s) files written: hello.txt
complete output: {"content": "Hello, world!\n", "path": "hello.txt"} || Created `hello.txt` (14 bytes). || Created `hello.txt` (14 bytes).
errors: none
--- second pass (after the redraw fix)
selected task activity lines: 37
detail title: 003 Live task backlog      (created with the form)
live task activity lines: 41, last: budget … | merge done … | ■ run finished: ready
status: ready, budget bars: 2, stream up: true, cursor: 1790543916305222000-3-41
feed mentions #3: 41
errors: none
```
Aucune exception JS n'a été relevée (`Runtime.exceptionThrown`). Le serveur et Chrome ont ensuite été arrêtés.

### Vérification finale (worktree step4, à `4820d9d` + modifications)
- `cargo fmt --all -- --check` : `fmt exit 0`.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` (via `rtk proxy`, après `touch`) : `clippy exit 0`, 0 ligne `warning` ou `error`. Dernières lignes :
  ```
      Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step4/crates/vibe-cli)
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.68s
  ```
- `cargo test --workspace --all-features` (via `rtk proxy`) : `test exit 0`, 28 lignes `test result`, **650 passés, 0 échec**. Suite `serve` : 11 tests. Les tests unitaires ajoutés sont `outputs_are_cut_on_a_character_boundary` et `only_event_streams_take_the_token_from_the_query`. Dernière ligne :
  ```
  test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  ```
- Rien n'est commité.
