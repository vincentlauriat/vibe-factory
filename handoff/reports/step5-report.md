# Étape 5 : TUI (rapport)

Travail fait dans le worktree `/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step5` (branche `feat/visibility-0.5-step5`, base `8d2f5ff`, avancée en fast-forward sur `4820d9d` (étape 3) avant les corrections de revue). Rien n'est commité. Je n'ai touché ni à la couche de lecture (`vibe-pipeline`) ni aux autres checkouts.

## Fichiers modifiés

| Fichier | Changement |
|---|---|
| `crates/vibe-cli/src/tui/mod.rs` | **Lecture incrémentale.** `SelectedLog` enveloppe un `EventReader` et repart de zéro si la tâche ou le run change. **Chargements en arrière-plan.** `Loader` et `Loaded` passent par une `mpsc` et un `tokio::spawn` (Activity, History, Trace, sortie complète). `project_history` utilise `commands::history::task_branch` (étape 3). `poll_feed` interroge l'`AllEventsFollower` à chaque tick. Tests. |
| `crates/vibe-cli/src/tui/app.rs` | `Screen` (Tasks/Activity/History), onglet `Tab::Trace`, `Load` et `take_loads()`, `set_feed`/`push_feed`/`set_history`/`set_trace`/`set_output`, `report_feed_error`, `describe_feed`, `restart_log`, touches par écran. `apply_log` ne reçoit plus que les événements nouveaux. `consumed` disparaît. `subtask_title` devient `pub`. Tests. |
| `crates/vibe-cli/src/tui/panes.rs` (nouveau) | État sans I/O : `LoadState`, `Group`, `Feed`/`FeedLine`, `HistoryView`, `history_row`, `history_detail`, `TraceData::from_events`, `TraceView`, `OutputView`, `call_row`, `call_detail`, `output_note`. Tests et fixtures (`fixture_history`, `call`). |
| `crates/vibe-cli/src/tui/view.rs` | Barre d'écrans dans le titre, `draw_feed`, `draw_history` (`Table`), `draw_trace` (liste, appel déplié, sortie complète), aide du pied de page selon le contexte (`help()`). Test de rendu `TestBackend` des trois écrans. |
| `docs/src/user/cli.md` | Section `vibe tui` seulement : trois écrans, onglet Trace, tables de touches. |

## A. Lecture des événements

- `try_refresh` charge d'abord `run.json`, puis `SelectedLog::read(store, task, run)`.
- **Quand le lecteur repart de zéro.** Il est recréé, à l'offset 0, dès que la tâche **ou** l'id du dernier run change. Il renvoie alors `restart = true`, et `App::restart_log()` efface l'activité, le texte en streaming et le budget. Le reste du temps, seuls les octets ajoutés sont lus.
- **Pourquoi aussi sur changement de run.** Il y a une course : les événements d'un nouveau run peuvent être lus pendant que `run.json` désigne encore l'ancien. Le filtre par run les écarterait alors pour de bon, sans cette remise à zéro.
- Les `AgentDelta` du bus en mémoire sont inchangés (`apply_live`).
- Test de mutation : sans la condition `self.run != run`, `the_selected_log_is_read_incrementally_and_again_on_a_new_selection` échoue. Code restauré ensuite.

## B. Écrans et touches (tels que documentés)

Titre : ` vibe · <projet> · N task(s), k running   T Tasks  A Activity  H History` (l'écran courant en vidéo inverse).

**Global** :
- `T`, `A` et `H` changent d'écran.
- `q` quitte, quel que soit l'écran.
- `Esc` ferme ce qui est ouvert : la sortie, puis l'appel déplié, puis le détail d'historique, puis on revient à Tasks. Il ne quitte que depuis le board de Tasks, comme avant, et jamais depuis l'onglet Trace (correction de revue 1).

**Tasks** : touches existantes inchangées (`↑↓ n r R c a x Tab g q`). J'ai ajouté `g` à la table de la doc : il existait déjà mais n'était pas documenté.

**Onglet Trace** (`Tab` fait maintenant le cycle Activity → Plan → Changes → Trace) :

| Touche | Action |
|---|---|
| `↑` `↓` (`j` `k`, `PgUp` `PgDn`) | choisir un appel ; dans un appel déplié ou sa sortie : défiler |
| `[` / `]` | run précédent / suivant |
| `Enter` | déplier l'appel : en-tête, note d'appariement, arguments en JSON indenté, aperçu, indication « o: complete output » |
| `o` | charger la sortie complète via `read_output` dans un panneau défilant. Notes : « not traced », « gone from .vibe/tool-output », « Paired by order » ; l'aperçu remplace la sortie quand elle manque |
| `Esc` | fermer la sortie, puis replier |
| `g` | recharger les appels |

- Colonnes de la liste : `seq role subtask tool duration exit err`, où `err` vaut `error`, `timeout` ou `no result`.
- Par défaut, l'onglet montre le run `trace::last_run`.
- `r R c a x n` continuent d'agir sur la tâche.

**Activity** :

| Touche | Action |
|---|---|
| `↑` `↓` (`PgUp` `PgDn`) | sélection ; le suivi s'arrête |
| `f` | suivi on/off |
| `1`…`7` | afficher ou masquer agents / tools / phases / git / approvals / budget / logs |
| `Enter` | ouvrir la tâche dans Tasks |
| `Esc` | revenir à Tasks |

- **Format d'une ligne** : `#003 HH:MM:SS <texte>`.
- **Barre de filtres** : elle est dans le titre du cadre. Un groupe masqué y est grisé et barré ; le cadre indique aussi `follow on/off`.
- **Entrée dans l'écran** : `AllEventsFollower::new(store, None)` et un premier `poll`, dans une tâche de fond. On garde les 200 derniers événements (`FEED_ON_ENTRY`), puis un `poll` à chaque tick tant que l'écran est affiché. Le tampon est borné à `FEED_LIMIT` = 2000.
- **Erreurs de lecture** d'une tâche : affichées une seule fois dans le pied de page, par couple (tâche, message) : « cannot read the log of task 003: … ».

**History** :

| Touche | Action |
|---|---|
| `↑` `↓` | choisir une ligne (dans le détail : défiler) |
| `Enter` | ouvrir le détail |
| `Esc` | fermer le détail, puis revenir à Tasks |
| `a` | ajouter ou retirer les tâches failed/cancelled (recharge) |
| `g` | recharger |

- **Colonnes** : `# title status runs commits files tokens active cost finished`.
- **Notation de `vibe history`** (correction de revue 2) : `+` en suffixe pour un minorant (tokens et active si `totals.complete` est faux, cost si `Cost.complete` est faux), `~` pour un nombre de fichiers approximatif. `finished` se replie sur `last_activity`.
- **Coût** : `-` sans table de prix.
- **Colonne `finished`** : temps relatif de la fin du dernier run.
- **Contenu du détail** :
  - runs : état, début en heure locale, durée, tokens, reprises ;
  - commits : SHA court, message, nombre de fichiers ; merges ;
  - fichiers modifiés : lettre de statut, source, `approximate` ;
  - validations, dernier QA, erreurs (`last_error` des runs et `TaskHistory.errors`).

## C. Chargements sans bloquer la boucle

- **`App::take_loads()`**, appelé avant chaque dessin :
  - il renvoie les chargements voulus par la vue affichée (`LoadState::Wanted`) ;
  - il les passe à `Loading(id)`, avec un id de requête global et croissant ;
  - `Loader::start` fait alors un `tokio::spawn`, et la réponse revient par une branche du `select!`.
- **Rechargement à l'entrée.** `LoadState::refresh()` repasse à `Wanted` en entrant sur History (`H`) ou sur l'onglet Trace (`Tab`). Il ne le fait pas si un chargement est en cours. Activity garde son follower et n'est pas reconstruit.
- **Réponses périmées.** Une réponse n'est prise que si l'état attend encore cet id. Sont donc ignorées :
  - la trace d'une tâche quittée entre-temps ;
  - un deuxième `g` ;
  - un changement de filtre `a`.
- **Affichage pendant le chargement.** « Loading… » s'affiche tant qu'il n'y a pas de données. Pendant un rechargement, les anciennes données restent visibles avec « · loading… » dans le titre.
- **Ce qui reste synchrone.**
  - Le `poll` du follower se fait sur le tick. Il est incrémental ; seul le premier, qui lit tout, part en tâche de fond.
  - `LoadChanges` reste synchrone, comme avant : hors périmètre.

## D. Tests (12 nouveaux, 1 adapté ; `tui` : 19 tests)

**`app.rs`** :
- `screens_switch_with_keys_and_esc_goes_back_before_quitting`
- `background_loads_are_asked_once_and_stale_answers_dropped` : il vérifie aussi le rechargement au retour sur History.
- `activity_filters_follow_and_jump_to_the_task`
- `feed_read_errors_are_shown_once`
- `the_trace_tab_expands_a_call_and_loads_its_output` : ↑↓ sur les appels, Enter, `o`, Esc ×2, puis Quit. Le retour sur l'onglet et un changement de tâche relancent le chargement.
- Adapté : `log_and_live_events_build_the_activity`. Il ne reçoit plus que les ajouts ; la non-duplication est maintenant couverte par le test du lecteur.

**`panes.rs`** :
- `history_rows_come_from_the_read_layer` : store temporaire créé via `FileTaskStore::open` + `save_task` + `events.jsonl`, comme `tests/history.rs`, puis `project_history`. Il vérifie la ligne exacte et le détail.
- `trace_data_defaults_to_the_last_run_and_switches_runs`
- `call_details_and_output_notes`
- `every_logged_event_has_a_group`

**`mod.rs`** :
- `the_selected_log_is_read_incrementally_and_again_on_a_new_selection` : ajout incrémental, autre tâche, retour à la tâche, nouveau run, `restart_log`.
- `the_history_branch_comes_from_the_task_or_the_worktree_provider`

**`view.rs`** :
- `activity_history_and_trace_screens_are_drawn` : `TestBackend`. Il dessine Activity, History (chargement, table, détail) et Trace (liste, appel déplié, sortie absente), et vérifie les chaînes clés et l'aide du pied de page. Je l'ai aussi regardé à l'œil, avec un `eprintln!` temporaire retiré depuis.

## Écarts et pourquoi

1. **Touche de retour à Tasks.** La consigne n'en nommait pas. J'ai pris `T` et `Esc`. `Esc` ne quitte plus que depuis Tasks, pour qu'un `Esc` de trop ne ferme pas l'UI depuis un sous-écran.
2. **↑↓ dans l'onglet Trace.** Ils déplacent la sélection d'appel, pas la tâche. C'est le seul changement d'une touche existante, et il est documenté : on sort de l'onglet par `Tab` pour changer de tâche.
3. **`a` sur History** bascule le filtre failed/cancelled au lieu d'approuver. C'est la consigne ; l'approbation reste sur Tasks.
4. **`describe_feed`** est un wrapper utilisé seulement par Activity. `describe()` n'a pas de ligne pour `committed`, `merged`, `budget_updated` ni les logs `info`, donc les filtres git et budget n'auraient rien montré. L'onglet Activity de Tasks reste inchangé.
5. **Groupes de la bascule `1`…`7`.** Deux groupes vont au-delà de leur nom :
   - « phases » contient aussi les runs, sous-tâches, validations, artefacts et pauses ;
   - « agents » contient aussi `retrying`.

   Les `tool_returned` réussis ne sont pas affichés, comme dans `describe()`.
6. **`task_branch`** : après le fast-forward, le TUI réutilise `commands::history::task_branch`, rendu `pub(crate)`. La copie locale et son test ont été supprimés.
7. **La Trace et l'History ne se rafraîchissent pas seules.** Elles se rechargent à chaque entrée (onglet Trace via `Tab`, écran History via `H`), au changement de tâche pour la Trace, et sur `g`, conformément à la consigne C. Si un chargement est déjà en cours, il n'est pas relancé (`LoadState::refresh`). Il n'y a pas de minuterie pendant qu'on reste sur la vue.
8. **Le follower Activity ne tourne que si l'écran est affiché.** Au retour sur l'écran, les événements accumulés arrivent d'un coup.
9. **Heures en heure locale** (`chrono::Local`) : dans les lignes Activity, les débuts de run et l'heure d'un appel. `chrono` était déjà une dépendance.
10. **Aucune modification de la couche de lecture**, aucune nouvelle dépendance.

## Vérification (dans le worktree)

```
$ cargo fmt --all -- --check
fmt exit 0

$ RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
    Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step5/crates/vibe-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.78s
clippy exit 0          (0 ligne « warning »)

$ cargo test --workspace --all-features
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

test exit 0            (28 suites « ok », 638 tests passés, 0 échec)
```

## Review fixes

Lot appliqué après la revue. Dans l'ordre :
1. Section `vibe tui` mise de côté.
2. `git checkout -- docs/src/user/cli.md`.
3. `git merge --ff-only 4820d9d` : HEAD est désormais `4820d9d`.
4. Section réappliquée sur le nouveau `cli.md`.
5. `task_branch` de l'étape 3 réutilisé.

Puis les 9 corrections :

1. **Esc dans l'onglet Trace** (`app.rs`, `trace_key`). Quand rien n'est ouvert, `Esc` ne fait plus rien au lieu de retomber dans le handler du board, qui quittait l'UI. Test adapté : `the_trace_tab_expands_a_call_and_loads_its_output`. On y vérifie qu'après sortie → repli → `Esc`, on reste sur Trace, et que `Esc` ne quitte que depuis un autre onglet.
2. **Notation de `vibe history`**. `lower_bound` et `cost_text` sont déplacés dans `util.rs`, où s'ajoutent `files_count` (`~`) et `finished_at` (repli sur `last_activity`). Les deux appelants les partagent : `commands/history.rs` et `tui/panes.rs::history_row`. Le test `lower_bounds_and_costs` suit dans `util.rs`. `history_rows_come_from_the_read_layer` vérifie `1~` et `1.5k+` / `1 min 05 s+`. `cli.md` est mis à jour.
3. **Redémarrage perdu sur erreur** (`SelectedLog::read`). Le nouveau lecteur, et avec lui `task` et `run`, n'est gardé qu'après un `read_new` réussi. Après un échec, l'appel suivant redémarre et renvoie encore `restart = true`. Test ajouté dans `the_selected_log_is_read_incrementally_and_again_on_a_new_selection` : `events.jsonl` y est remplacé par un répertoire, puis restauré. **Mutation** : en revenant à l'ancien ordre, le test échoue ; code restauré ensuite.
4. **Relance d'Activity**. Il suffit maintenant d'entrer sur l'écran, avec `A` depuis un autre écran, ou de faire `g` dans l'écran : `feed.load.refresh()`. Un chargement en échec est donc relancé. Test : `activity_is_loaded_again_on_entry_on_g_and_after_a_failure`.
5. **Erreur d'index affichée une fois**. `App::report_index_error(Option<String>)` et `Feed.index_error` : le message n'est répété que si l'erreur change ou après un poll réussi. Test : `index_errors_are_shown_once_until_they_change`.
6. **Pas de rattrapage synchrone**. Revenir sur Activity relance un chargement en tâche de fond (point 4). Au lancement d'un `Load::Activity`, la boucle abandonne l'ancien follower, pour que le tick ne rattrape pas l'arriéré en synchrone. Le nouveau follower arrive avec les 200 derniers événements.
7. **Sortie complète**. `OutputView.lines` découpe le texte une seule fois au chargement ; l'aperçu le remplace si la sortie manque, via `OutputView::set`. Le rendu n'emprunte que les lignes visibles (`skip(scroll).take(height)`), avec `Wrap { trim: false }`. `scroll` passe en `usize`, borné à la dernière ligne par `scroll_by`. Test : le défilement s'arrête à la dernière ligne.
8. **Sortie ouverte**. `g` ferme la sortie et recharge les appels, au lieu du board. `Tab` ferme la sortie puis change d'onglet. Test dans `the_trace_tab_expands_a_call_and_loads_its_output`.
9. **`OutputView.index` supprimé.**

`docs/src/user/cli.md` précise aussi :
- Esc sur l'onglet Trace ;
- `g` sur Activity ;
- le rechargement à l'entrée pour Activity, History et Trace.

Fichiers touchés par ce lot, en plus du TUI :
- `crates/vibe-cli/src/util.rs` : helpers partagés et test ;
- `crates/vibe-cli/src/commands/history.rs` : helpers déplacés, test retiré, `task_branch` en `pub(crate)`.

### Vérification après corrections

```
$ cargo fmt --all -- --check
fmt exit 0

$ RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
    Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step5/crates/vibe-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.92s
clippy exit 0          (0 ligne « warning »)

$ cargo test --workspace --all-features
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

test exit 0            (28 suites « ok », 654 tests passés, 0 échec ; le TUI en compte 20)
```

## Review fixes, détails 10 à 13

10. **Entrée dans Activity à mémoire bornée** (`tui/mod.rs`, `last_events`). Les logs sont lus un par un, et on ne garde que leurs 200 derniers événements. On fusionne ensuite par curseur et on garde les 200 plus récents. Le pic mémoire est donc le plus gros log, et non tout l'historique.
    - Le follower est `AllEventsFollower::new(store, Some(curseur du plus récent gardé))`. Son premier `poll`, fait dans la même tâche de fond, rattrape ce qui a été écrit pendant la lecture, sans trou ni doublon.
    - Je n'ai pas pris `from_end` : il obligeait à relire les fins de fichiers à part, avec une course entre les deux lectures.
    - Test : `activity_opens_on_the_last_events_and_follows_the_next_ones`. Il vérifie les 4 plus récents sur 8, l'ordre, l'absence de doublon, puis un ajout suivi.
11. **Recul de l'offset = redémarrage** (`SelectedLog::read`). Si `reader.offset()` recule pendant `read_new` (log tronqué), c'est traité comme un restart et l'activité affichée est effacée. Test : un log de 2 lignes réécrit avec 1 ligne. **Mutation** : sans la condition, le test échoue ; code restauré ensuite.
    - Limite : un log remplacé par un fichier plus long que ce qui avait été lu n'est pas vu ici. L'`EventReader` relit bien depuis 0, mais il n'expose pas son identité de fichier, et je n'ai pas modifié la couche de lecture.
12. **Largeur affichée dans `call_row`.** `unicode-width` n'est pas une dépendance directe. J'ai utilisé `console::measure_text_width`, déjà utilisé par `util::Table`, dans un nouveau helper `panes::fit(text, width)` qui coupe (`…`) et complète à la largeur en colonnes, CJK et emoji comptant double. Test : `cells_are_fitted_to_the_displayed_width`.
13. **Rendu en petites tailles** (`every_screen_draws_at_tiny_sizes_without_panicking`). Toutes les tailles de 0×0 à 40×8 sont testées, sur 10 états :
    - les 4 onglets ;
    - l'appel déplié ;
    - la sortie de 50 lignes ;
    - Activity ;
    - History en échec, puis avec le détail ouvert.

    Aucun panic.

### Vérification finale

```
$ cargo fmt --all -- --check
fmt exit 0

$ RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
    Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory-step5/crates/vibe-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.85s
clippy exit 0          (0 ligne « warning »)

$ cargo test --workspace --all-features
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

test exit 0            (28 suites « ok », 657 tests passés, 0 échec ; le TUI en compte 23)
```
