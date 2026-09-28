# Étape 3 : CLI (`vibe events` global, `vibe history`, `vibe trace`)

Tout est dans l'arbre de travail de `feat/visibility-0.5`. Rien n'est commité. Hors CLI de l'étape 3, j'ai seulement ajouté `vibe serve --exit-on-stdin-eof`, décrit en fin de rapport. Je n'ai touché ni `vibe-pipeline` (la couche de lecture est inchangée), ni `apps/`, `.github/`, `PLAN.md`, `TODOS.md`, `CHANGELOG.md` ou `README.md`. Au moment du `git status`, `TODOS.md` apparaît modifié : ce n'est pas de mon fait, un autre agent y a écrit.

## Fichiers modifiés

**Nouveaux fichiers**
- `crates/vibe-cli/src/commands/history.rs` : `vibe history`.
- `crates/vibe-cli/src/commands/trace.rs` : `vibe trace`.

**Fichiers modifiés**
- `crates/vibe-cli/src/cli.rs` :
  - dans `EventsArgs`, `REF` devient optionnel, `--after` et `--all` exigent `REF`, et trois options arrivent : `--since`, `--type` (répétable, validée par `PossibleValuesParser::new(Event::TYPES)`) et `--task` (répétable) ;
  - `parse_since` et `parse_since_at` sont nouveaux ;
  - `HistoryArgs` et `TraceArgs` sont ajoutés, ainsi que les variantes `Command::History` et `Command::Trace` ;
  - 2 tests unitaires.
- `crates/vibe-cli/src/commands/events.rs` : mode tâche unique conservé, avec en plus `--since` et `--type`. Le mode global est nouveau (`every_task` et `Feed`, un `Renderer` par tâche, le préfixe `#N`). 1 test unitaire.
- `crates/vibe-cli/src/commands/mod.rs` : dispatch, et `subtask_labels(store, task)` (« pos/total titre » par `SubtaskId`, partagé par history et trace).
- `crates/vibe-cli/src/commands/task.rs` : `task show` affiche `branch` depuis `task.branch` (livrable D). Avant, la branche n'apparaissait que si git la trouvait encore.
- `crates/vibe-cli/src/render.rs` : `Renderer::text(&Envelope) -> Option<String>` (public), pour préfixer les lignes sans que le renderer imprime lui-même.
- `crates/vibe-cli/src/util.rs` : `print_out(&str) -> Result<bool>`. Elle renvoie `Ok(false)` sur `BrokenPipe`, pour que `vibe … | head` sorte avec le code 0 au lieu de paniquer.
- `crates/vibe-cli/tests/cli.rs` : `help_lists_every_command` inclut maintenant events, history et trace. S'y ajoutent 3 tests de bout en bout et leurs helpers (`greeting_script`, `project_with_two_runs`, `stdout_of`, `json_lines`).
- `docs/src/user/cli.md` :
  - synopsis ;
  - section `vibe events` réécrite ;
  - nouvelles sections `vibe history` et `vibe trace` ;
  - `task show` mentionne la branche ;
  - une tâche jamais lancée : `Task has not been run yet.`, ou `null` / `[]` avec `--json` ;
  - le paragraphe « JSON output » est complété.

## Référence des commandes (telle que documentée)

```
vibe events <REF> [--after SEQ] [--follow | --all] [--since TIME] [--type TYPE]...
vibe events [--follow] [--since TIME] [--type TYPE]... [--task REF]...
vibe history [--all]
vibe history <REF>
vibe trace <REF> [--run RUN | --all] [--tool NAME] [--subtask ID] [--full]
```

### `vibe events` sans REF

Le flux fusionne toutes les tâches par `EventCursor`. Chaque ligne est préfixée `#N`, et ce préfixe se place après le saut de ligne qui ouvre les lignes `── phase ──`.

- **`--json`.** Un `TaggedEnvelope` par ligne : `{"task","number","schema","seq","at","event"}`.
- **`--follow`.** Sans `--since`, il part de `AllEventsFollower::from_end` ; avec `--since`, de `AllEventsFollower::new(store, Some(EventCursor::since_time(t)))`. Il s'arrête sur Ctrl-C avec le code 0, et le handler n'est installé qu'en mode follow.
- **Erreurs propres à une tâche.** `PollResult.errors` est signalé une seule fois par tâche sur stderr (`warning: cannot read the events of task <short>: …`), sans arrêter le flux.
- **`--since`.** Accepte RFC 3339 ou un âge `Ns`, `Nm`, `Nh`, `Nd` ; l'unité est obligatoire.
- **Filtres.** `--type` et `--task` filtrent. Avec `REF`, `--since` et `--type` s'appliquent aussi.

### `vibe history`

- **Sans REF.** Table `# title status runs commits files tokens active cost finished` :
  - `tokens` vaut entrée plus sortie ;
  - `+` marque une borne inférieure (`totals.complete` ou `cost.complete` faux), `~` une liste de fichiers approximative, avec une légende sous la table ;
  - `cost` s'affiche avec 2 décimales et la devise, `-` sans tarif ;
  - `finished` est le dernier `finished_at`, sinon `last_activity`, en temps relatif ;
  - les erreurs de chaque tâche vont sur stderr ;
  - `--all` ajoute les tâches Failed et Cancelled (via `HistoryFilter`).
- **`branch_of`.** Prend `task.branch`, sinon `worktree_location(root, task).branch`, mais seulement si `pipeline.workspace` utilise des worktrees ; sinon `None`.
- **Avec REF (n'importe quelle tâche), le détail :**
  - en-tête : statut, branche, tokens, temps actif, coût, activité ;
  - un bloc par run : état running/finished/interrupted et statut final, début, nombre de reprises, temps actif, tokens, coût du run (`cost_of` sur les événements du run, seulement si la tâche a un coût), phases ✓/✗/…, merge, approbation en attente, dernière erreur. Les totaux inconnus sont affichés comme tels ;
  - commits : sha court, première ligne du message, nombre de fichiers, sous-tâche par son titre du plan ;
  - fichiers : lettre A/M/D/R/C/?, source, « approximate » ;
  - validations du dernier run qui en a ;
  - dernier QA ;
  - « Problems » (`TaskHistory.errors`).
- **`--json`.** `TaskHistory` ou `[TaskHistory]`, sérialisés tels quels.

### `vibe trace`

Un en-tête `── run <short> ──` par run, puis un bloc par appel :

```
#<position> <role> · subtask <pos/total titre> · <tool>  <durée>  <ok|exit N|error, exit N|error|timed out|no result>  [call <id|->(, paired: by order)]
    <arguments en JSON indenté ; chaînes > 2000 caractères coupées « … (N more chars, --full) » sauf avec --full>
  ⟵ output
    <aperçu, ou sortie complète avec --full>
```

- **Sortie.** Rouge en cas d'erreur. L'aperçu est annoncé comme tel quand `output_chars` dépasse sa longueur, avec « --full to read them » si un fichier existe.
- **`--full`.** Chaque appel est traité à part, sans jamais faire échouer la commande : « output not traced (tracing off, or logged before 0.5) », « output file missing (discarded or cleaned up) », ou « cannot read the output: … » quand `read_output` renvoie `Err`.
- **Pied de run.** `N call(s), E error(s)` et `files written: …`, recalculé sur les appels filtrés.
- **Sélection et filtres.** `--run` et `--subtask` acceptent un préfixe ; un préfixe introuvable ou ambigu donne le code 1. `--all` couvre chaque run, dans l'ordre d'apparition dans le journal.
- **`--json`.** `RunTrace` (ou `[RunTrace]` avec `--all`), filtres appliqués. Une tâche jamais lancée donne `null` ou `[]`.

## Écarts et raisons

1. **`--task` combiné avec `REF` donne l'intersection.** La consigne disait « allowed in both modes » : si `REF` ne fait pas partie des `--task`, rien n'est imprimé. Refuser la combinaison aurait contredit la consigne ; l'ignorer aurait surpris.
2. **`#seq` du bloc de trace.** C'est la position 1-based de l'appel dans le run, suivie du call id, car `Call` n'a pas de numéro de séquence. Elle reste stable sous les filtres. Je n'ai rien ajouté à la couche de lecture, dont la forme JSON sera consommée par le serveur et l'app macOS.
3. **`--since` a son propre parseur (`parse_since`).** `parse_duration_secs` reste inchangé : le test `budget_flags` exige que `"3d"` soit refusé pour `--max-duration`. Un nombre sans unité est refusé pour `--since`, car ambigu.
4. **Branche affichée dans `history <REF>`.** On affiche `task.branch`, sinon la branche confirmée par git (`ChangedFilesSource::Branch`). La branche dérivée par le provider n'est qu'une supposition : un run `in_place` affichait une branche qui n'existe pas. `branch_of` passe quand même la branche dérivée à la couche de lecture, qui vérifie son existence.
5. **`--full` ne change pas le JSON de `trace`.** Le `RunTrace` reste identique pour le round-trip. Le client lit `output_file`. C'est documenté.
6. **Mode global sans `--follow`.** Il fait un seul `poll()` d'`AllEventsFollower::new(store, after)` plutôt qu'`all_events()`. `all_events` échoue au premier journal illisible, alors que le follower renvoie les erreurs tâche par tâche : le comportement reste le même, avec ou sans `--follow`.
7. **Mode tâche unique avec `--follow`.** La fin de run (`run_finished` du run suivi) est détectée avant les filtres `--after`, `--since` et `--type`. Changement mineur : auparavant, un `--after` supérieur au seq de `run_finished` suivait indéfiniment.
8. **Codes de sortie inchangés.** La table n'est pas modifiée. Une valeur `--type` invalide sort en 2, comme toute erreur d'usage clap, ce qui existait déjà pour toutes les commandes.
9. **SIGPIPE.** `print_out` est utilisé par les nouvelles sorties (`history`, `trace`, flux global). Le mode tâche unique imprime toujours via `Renderer::on_event` (`println!`), comme avant.
10. **Smoke.** Le provider mock par défaut n'appelle aucun outil : `vibe trace 1` y est vide. J'ai ajouté une tâche 2 scriptée (`write_file` plus un `bash` en échec) pour montrer un bloc réel.

## Smoke (projet temporaire, `NO_COLOR=1`)

Tâche 1 : `vibe run 1 --provider mock` (git_worktree), sortie 0. Tâche 2 : `vibe run 2 --workspace in_place --script script.json`, sortie 0.

```
$ vibe history
#  title                      status  runs  commits  files  tokens  active  cost  finished
2  Write the greeting module  ready   1     1        1~     5.4k    56 ms   -     just now
1  Fix typo in README         ready   1     0        0      2.9k    176 ms  -     just now
~ approximate file count

$ vibe history 1
#1 Fix typo in README
  status    ready
  branch    vibe/fix-typo-in-readme-0015c732
  tokens    2.7k in / 165 out
  active    176 ms
  activity  just now

Runs (1)
  641af30c finished → ready, started just now
    176 ms active, 2.7k in / 165 out tokens
    phases  assess ✓  plan ✓  build ✓  qa ✓  merge ✓

Files (0) from nothing recorded

QA round 1: approved, 0 issue(s)
  Approved by the mock provider.

$ vibe trace 1 | head -40
── run 641af30c ──────────

0 call(s), no error

$ vibe trace 2 | head -40
── run a85d30e4 ──────────

#1 coder · subtask 1/1 Write hello.txt · write_file  0 ms  ok  [call dbbd295e4534]
    {
      "content": "Hello, world!\n",
      "path": "hello.txt"
    }
  ⟵ output
    Created `hello.txt` (14 bytes).

#2 coder · subtask 1/1 Write hello.txt · bash  8 ms  error, exit 1  [call ab73f14f4e73]
    {
      "command": "cat hello.txt; ls nope"
    }
  ⟵ output
    Hello, world!
    ls: nope: No such file or directory
    
    Exit code: 1

2 call(s), 1 error(s)
files written: hello.txt

$ vibe events --since 1h | head -20
#1 ▶ run 641af30c

#1 ── assess ──────────
#1 ✓ assess: complexity trivial (heuristic); phases: plan → build → qa → merge (0 ms)

#1 ── plan ──────────
#1   ● planner
#1   ● planner finished: 1 step(s), 764 in / 93 out tokens, completed
#1 ✓ plan: 1 subtask(s) in 1 phase(s) (0 ms)

#1 ── build ──────────
#1   ▸ subtask 1/1 Implement the task: in_progress
#1   ● coder · subtask 1/1 Implement the task
#1   ● coder finished: 1 step(s), 919 in / 41 out tokens, completed
#1   ▸ subtask 1/1 Implement the task: done
#1 ✓ build: 1 done, 0 failed, 0 skipped (0 ms)

#1 ── qa ──────────
#1   ● qa_reviewer
#1   ● qa_reviewer finished: 1 step(s), 1.0k in / 31 out tokens, completed
(exit 0)
```

Vérifications manuelles supplémentaires :
- `vibe events --follow --since 1h --type run_finished` en arrière-plan, puis `kill -INT` : les 3 `run_finished` sont imprimés, sortie 0.
- `vibe events --follow --type run_started --type run_finished` lancé avant un nouveau run : imprime seulement `#3 ▶ run …` et `#3 ■ run finished: backlog`.
- `vibe events --type bogus` : erreur clap listant les types valides, sortie 2.
- Journal illisible : `events.jsonl` de la tâche 2 remplacé par un répertoire. `vibe events` sort en 0 ; les 20 lignes de #1 sont imprimées et aucune de #2 ; stderr porte une seule ligne, `warning: cannot read the events of task 50b4d653: Storage: Is a directory (os error 21)`. Même résultat avec `--json` (32 lignes, un seul avertissement).

## Tests ajoutés (9)

**Unitaires**
- `cli::tests::since_accepts_times_and_ages` : RFC 3339 avec décalage, `30m`, `2H`, `1d`, `45s`. Sont refusés `""`, `d`, `30`, `3w`, `-2h`, `yesterday`, `2026-09-27` et un débordement.
- `cli::tests::events_flags_and_type_validation` :
  - `--type` et `--task` répétables, un type inconnu refusé ;
  - `--after` et `--all` sans REF refusés, `REF --all --type` accepté ;
  - `trace --run … --all` et `history REF --all` refusés.
- `events::tests::tags_go_after_leading_blank_lines`.
- `history::tests::lower_bounds_and_costs`.
- `trace::tests::long_strings_are_cut_by_characters` : UTF-8 multi-octets, tableaux imbriqués.
- `trace::tests::prefixes_must_match_one_id` : unique, ambigu, absent, vide.

**Bout en bout** (`tests/cli.rs`) : la tâche 1 est scriptée en place avec `write_file`, un commit et `ready` ; la tâche 2 est en `--dry-run` mock.
- `history_lists_finished_tasks_with_their_commits_and_files` :
  - la table contient #1 et pas #2 (backlog) ;
  - `--json` : un seul élément, `state` finished, `commits` ≥ 1, `hello.txt` dans `changed_files`, `cost` null ;
  - `history 1` en JSON et en texte : runs, commits, message, fichier, QA ;
  - `history 2` fonctionne pour une tâche non terminée.
- `trace_lists_calls_and_reads_full_outputs` :
  - `write_file`, arguments en JSON, `files written`, call ids en 12 hex ;
  - `--json` désérialisé en `vibe_pipeline::RunTrace` puis resérialisé à l'identique ;
  - `--full` affiche une sentinelle écrite dans `output_file`, que le mode par défaut n'affiche pas ; après suppression du fichier, « output file missing » ;
  - `--tool` filtre ; `--run <8 premiers caractères>` fonctionne ; `--all` renvoie un tableau ; `--run zzzz` sort en 1.
- `global_events_are_tagged_filtered_and_ordered` :
  - chaque ligne JSON a `task` et `number`, les numéros sont {1, 2} et `at` est croissant ;
  - `--type run_finished` donne exactement 2 lignes, toutes de ce type ;
  - `--task 2` ne donne que des numéros 2 ;
  - `--since 1h` renvoie tout, un `--since` dans le futur ne renvoie rien ;
  - en texte, des lignes commencent par `#1 ` et `#2 ` ;
  - `events 1 --type run_finished` donne une ligne sans `task` (forme inchangée) ;
  - `--type run_done` sort en 2.
- `events_replays_numbered_events_and_follows_to_the_end` (existant) passe toujours : `vibe events <REF>` est inchangé.

## Vérification

`cargo fmt --all -- --check` :
```
fmt exit 0
```

`RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` (via `rtk proxy`) :
```
    Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.65s
clippy exit 0
```

`cargo test --workspace --all-features` (via `rtk proxy`) : code de sortie 0, 28 lignes `test result`, 635 tests passés, 0 échec, aucun panic (626 avant, 9 ajoutés). Dernière ligne :
```
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Ajout : `vibe serve --exit-on-stdin-eof`

- **`cli.rs`.** Nouveau champ `ServeArgs.exit_on_stdin_eof`.
- **`server/mod.rs`.** Le signal d'arrêt est factorisé dans `stop_signal(Option<oneshot::Receiver<()>>)`, qui attend soit Ctrl-C, soit la fin de stdin. Il est passé à `with_graceful_shutdown`, et l'arrêt qui suit ne change pas :
  - annulation des runs, avec `SHUTDOWN_GRACE` ;
  - suppression de `.vibe/server.token` ;
  - `ctx.shutdown()` ;
  - code de sortie 0.
- **Lecture de stdin.** `stdin_eof()` lit stdin sur un `std::thread` et non sur le pool bloquant de tokio : une lecture de stdin bloquée dans ce pool retarderait l'arrêt du runtime après un Ctrl-C. Le fil signale l'EOF ou une erreur de lecture via le oneshot.
- **Sans le flag,** stdin n'est pas lu et le comportement est inchangé.
- **Doc.** Dans `cli.md`, section `vibe serve` : synopsis et un paragraphe.
- **Test** (`tests/serve.rs`), `closing_stdin_stops_the_server_with_exit_on_stdin_eof` :
  - lance `vibe --json serve --exit-on-stdin-eof --port 0 --provider mock` avec stdin en pipe ;
  - lit la ligne JSON et vérifie que `server.token` existe ;
  - ferme stdin ;
  - vérifie que le processus sort avec le code 0 en moins de 5 s et que le fichier de jeton est supprimé.
- **Contrôle manuel.** Stdin branché sur `/dev/null` :
  - sans le flag, le serveur tourne encore après 2 s, puis s'arrête sur Ctrl-C avec le code 0 ;
  - avec le flag, il est déjà arrêté, avec le code 0.

### Vérification après l'ajout (remplace la précédente)

- `cargo fmt --all -- --check` : sortie 0.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` : sortie 0. Dernière ligne : `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1.31s`.
- `cargo test --workspace --all-features` : sortie 0, 28 lignes `test result`, **636 passés** (635 + 1), 0 échec, aucun panic.

## Review fixes

Pour les points 1 à 4 et le point 8, j'ai prouvé chaque test par mutation : le correctif retiré localement, le test échoue ; le correctif rétabli, il passe. Les sorties avec mutation sont listées plus bas.

1. **MAJOR : `events REF --follow` sur un run repris** (`commands/events.rs`). Un `run_started` du run suivi remet `ended` à faux, puisque la reprise réutilise le run id.
   - Test `following_a_resumed_run_waits_for_its_new_end` (unix) :
     - lance `run 1 --until plan`, puis `run 1 --resume` ;
     - vérifie que les deux `run_started` portent le même run id ;
     - coupe le journal juste après le 2e `run_started` ;
     - lance `events 1 --follow`, qui tourne encore après 1,5 s ;
     - rajoute la suite du journal : la commande sort en 0 et imprime 2 fois « run finished ».
   - Avec mutation (ligne `RunStarted => ended = false` retirée) : `FAILED`.
2. **MINOR : `print_json` et une sortie standard fermée** (`util.rs`). `Ui::print_json` passe par `print_out`, donc une sortie fermée n'est plus une panique.
   - Test `a_closed_standard_output_is_not_a_crash`. Le pipe stdout est fermé avant que vibe écrive, pour ces 7 commandes, qui doivent toutes sortir en 0 :
     - `--json history`, `--json history 1`, `--json trace 1` ;
     - `trace 1` ;
     - `events 1`, `--json events 1` ;
     - `events`.
   - Avec mutation (`println!`) : `FAILED`.
3. **MINOR : `events REF` et une sortie standard fermée.** Le mode tâche unique formate via `Renderer::text` (ou `serde_json` avec `--json`) et imprime via `print_out` ; `Renderer::on_event` n'y sert plus.
   - Même test que le point 2 ; avec mutation (`println!` à la place de `print_out`) : `FAILED`.
4. **MINOR : `events --follow` sans `--since` et un journal illisible.** Correctif dans `vibe-pipeline/src/events_log.rs` : `AllEventsFollower::from_end` ne fait plus échouer l'ensemble pour un journal illisible.
   - La tâche est marquée non positionnée (champ privé `unpositioned: BTreeSet<TaskId>`).
   - Chaque `poll` retente `EventReader::at_end` : l'erreur est remontée dans `PollResult.errors` tant que le journal est illisible, puis le lecteur est placé à la fin du journal réparé.
   - Seule une erreur d'index reste fatale.
   - **Écart par rapport à la suggestion « reader à 0 ».** Un lecteur à 0 rejouerait tout l'historique de la tâche au moment de la réparation. Cela contredirait « from now on ».
   - Test pipeline `a_follower_from_the_end_reports_an_unreadable_log_and_goes_on` : erreur au 1er poll, événements des autres tâches livrés, réparation sans rejouer l'ancien contenu, lignes suivantes livrées.
   - Test CLI `following_every_task_survives_an_unreadable_log_and_sees_new_tasks` (unix) : journal de #2 remplacé par un répertoire, puis `events --follow` en arrière-plan. Vérifié :
     - le suivi tourne encore après 1 s ;
     - une tâche #3 créée et lancée pendant le suivi apparaît (`#3 ▶ run`) ;
     - aucune ligne de #1 ne remonte (le suivi part de maintenant) ;
     - SIGINT fait sortir en 0 ;
     - l'avertissement apparaît exactement une fois sur stderr.
   - Avec mutation (`?` rétabli dans `from_end`) : le pipeline échoue sur `unwrap` ; la CLI échoue avec `stopped: error: Storage: Is a directory (os error 21)`.
5. **MINOR : doc du coût** (`cli.md`). La formulation proposée (« + quand certains modèles n'ont pas de tarif ») ne correspond pas au code : `cost_of` renvoie `None` dès qu'un modèle n'a pas de prix, donc `-`. Le texte écrit :
   - `-` sans table `[pricing]`, ou quand un modèle utilisé n'y a pas de prix ;
   - `+` sur le coût marque une borne inférieure : des tokens consommés hors d'une session d'agent terminée (session coupée par un crash, réparation d'une réponse structurée) ne sont pas tarifés ;
   - `+` sur tokens et temps actif : run journalisé avant 0.5 ou non terminé.

   La légende de la table couvre aussi un coût partiel, avec des totaux complets : « + lower bound (runs logged before 0.5 or unfinished, tokens not priced) ».
6. **NIT : lecture de stdin du serveur** (`server/mod.rs`). La boucle continue sur `ErrorKind::Interrupted`. Pas de test : un EINTR n'est pas provoquable de façon fiable.
7. **NIT : Ctrl-C pendant la relecture initiale** (`events.rs`). Le handler est armé avant le premier `poll` en mode follow. Entre deux événements imprimés, `now_or_never()` le vérifie, si bien qu'un Ctrl-C pendant une longue relecture sort en 0. Sans follow, rien n'est installé. Couvert pour le cas nominal par le test SIGINT du point 4 ; pas de test dédié pendant un gros dump.
8. **NIT : `trace --full` et un appel sans résultat** (`trace.rs`). Il affiche « no result: the call never returned (run interrupted) » au lieu de « output not traced ».
   - Test : fin de `trace_lists_calls_and_reads_full_outputs`. Les `tool_returned` sont retirés du journal ; `trace 1 --full` contient « no result » et pas « output not traced ».
   - Avec mutation : `FAILED`.
9. **Tests supplémentaires :**
   - follow global plus tâche créée pendant le suivi plus SIGINT : voir le point 4 ;
   - `history_outside_git_uses_the_trace` : projet sans git, run scripté en place ; `history --json` donne `commits` 0 et `changed_files.source.kind` = `trace` avec `hello.txt` ; la table et le détail affichent « from files written by the agents » ;
   - `events 1 --task 2` sort en 0 sans rien imprimer, ajouté à `global_events_are_tagged_filtered_and_ordered`.

**Point de méthode.** Restaurer un fichier muté avec `mv` de sa copie a remis un mtime antérieur au build muté, donc cargo a gardé le binaire muté. Le premier `cargo test --workspace` a ainsi échoué, faussement, sur `following_every_task…`. Après un `touch` des 4 fichiers et une recompilation, tout passe (ci-dessous).

Nouveaux helpers de test dans `tests/cli.rs` : `Project::vibe_process()` (le même environnement que `vibe()`, en `std::process::Command`) et `wait_for(child, secs)`.

### Vérification après les corrections (remplace les précédentes)

- `cargo fmt --all -- --check` : sortie 0.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` : sortie 0. Dernières lignes :
  ```
      Checking vibe-pipeline v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-pipeline)
      Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-cli)
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.11s
  ```
- `cargo test --workspace --all-features` : sortie 0, 28 lignes `test result`, **641 passés** (636 plus 5 nouveaux : 1 pipeline, 4 CLI), 0 échec, aucun panic. Dernière ligne : `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`.
