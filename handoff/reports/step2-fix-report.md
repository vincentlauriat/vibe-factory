# Étape 2 : corrections de revue

Les 8 corrections sont faites dans l'arbre de travail ; rien n'est commité, rien n'a été modifié hors de `crates/` et `docs/`.

## Changements et preuves

1. **Fast-forward vers un merge d'intégration** (`history.rs`, `Repo::merge_files`). `fast_forward` vaut maintenant `parents < 2 || merge.commit ∈ commits de la tâche`.
   - Test : `a_fast_forward_to_an_integration_merge_lists_every_subtask`.
   - Avant la correction, il échouait : seul `sub.rs` était listé, et le merge était pris pour un vrai merge. Après, il passe avec `other.rs` et `sub.rs`.
2. **Commits perdus lors d'un reset.** On prend désormais le plus ancien commit enregistré qui est un ancêtre du merge, via un nouveau `Git::is_ancestor` (`vibe-workspace/src/git.rs`, `merge-base --is-ancestor`). S'il n'y en a aucun : `<commit>^1` avec `approximate = true`.
   - Test : `commits_lost_to_a_reset_do_not_widen_the_merge_diff`.
   - Avant, il échouait : `base.rs` de la base apparaissait dans la liste. Après, il passe.
3. **`AllEventsFollower::poll` renvoie `Result<PollResult { events, errors: Vec<(TaskId, Error)> }>`.** Seule une erreur d'index reste fatale. Le lecteur en erreur ne bouge pas, les autres livrent leurs événements.
   - Test : `an_unreadable_log_does_not_lose_the_events_of_the_others`, avec des ids fixes pour que le log illisible soit lu au milieu.
   - Avant, avec l'ancienne API et un test temporaire depuis supprimé : les deux polls renvoyaient `Err("Is a directory")`, les événements de la tâche a, déjà lus, étaient perdus, et ceux de c n'étaient jamais livrés.
4. **Erreurs propres à une tâche** (`events.jsonl`, `run.json`, `qa_report_*.json`, verrou) : elles vont dans `TaskHistory.errors`. Seules les erreurs d'index (`entry`, `list_tasks`) restent fatales.
   - Test : `one_unreadable_task_does_not_fail_the_project_history`.
   - Avant, il échouait : `Err(cannot parse …/run.json)`. Après, il passe avec 2 erreurs sur la tâche.
5. **`apply_run_state`** prend aussi les totaux de `run.json` quand `finished_at` est `None` et que `run.json` est plus grand.
   - Test : `run_json_updates_the_totals_of_a_resumed_run_that_did_not_finish`. Il échouait avant (10/5 gardé au lieu de 30/9) et passe après.
6. **`cost_of`** : un `run_started` supprime les sessions encore ouvertes de ce run et met `complete = false`. La limite restante est documentée sur `cost_of` : des sessions parallèles d'un même rôle, dans un même processus, sont appariées dans l'ordre.
   - Test : `a_session_lost_to_a_crash_is_not_paired_after_the_resume`. Il échouait avant (montant 30 au lieu de 3) et passe après.
7. **`EventCursor { at, number, seq }`** : `Ord`, `serde`, et `Display`/`FromStr` sous la forme `<nanos>-<number>-<seq>`, utilisable comme id SSE ; `seq` vaut 0 quand il est absent. S'y ajoutent `TaggedEnvelope::cursor()`, `EventCursor::since_time(at)`, `all_events(after: Option<EventCursor>)` et `AllEventsFollower::new(store, after)`. Le tri et le filtre se font par curseur.
   - Test ajouté dans `all_events_merge_every_task_by_time` : aller-retour texte du curseur, et reprise après l'événement de la tâche 1 à `at(20)` qui livre celui de la tâche 2 au même instant. Avec l'ancien filtre `at > since`, cet événement était perdu.
8. **`RunSummary.state: RunSummaryState { Running, Finished, Interrupted }`.** Un run avec `run_finished` depuis son dernier démarrage est `Finished`, les autres sont `Interrupted`. Le dernier run passe à `Running` si `FileTaskStore::is_running` le dit.
   - Test : `runs_without_run_finished_are_running_or_interrupted`, avec le verrou tenu puis relâché.

Pour les points 7 et 8, les tests portent sur une API nouvelle : ils ne compilaient pas avant la correction. Pour 7, la preuve est l'événement au même instant perdu par l'ancien filtre, comme décrit au point 7.

## Points d'attention

- `is_running` prend brièvement le verrou. Un `vibe run` qui démarre exactement pendant cet instant échouerait avec « already being run ». La fenêtre ne s'ouvre que si le dernier run n'a pas de `run_finished`.
- Un `task.json` corrompu fait encore échouer `list_tasks`, donc tout l'historique. C'est une lecture de niveau index : je n'y ai pas touché.

Les docs de `docs/src/design/persistence.md` (curseur, erreurs de poll, état des runs, règle du fast-forward, erreurs par tâche) sont mises à jour.

## Vérification

- `cargo fmt --all -- --check` : code de sortie 0.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features`, après `touch` des sources modifiées : code de sortie 0, aucun avertissement ; dernière ligne `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 3.19s`.
- `cargo test --workspace --all-features` : code de sortie 0, 28 suites, 623 tests passés, 0 échec, aucun panic.
  - J'ai ajouté 7 tests (6 dans `tests/history.rs`, 1 dans `events_log`) et étendu un test existant pour le point 7.
  - Le total précédent était de 615 ; je n'ai pas cherché d'où vient le huitième test en plus.

## Compléments de revue (points 9 à 17)

Le détail de chaque point est dans `step2-fix2-report.md`. En résumé :

- **9.** `persistence.md` décrit les limites du fast-forward et le sens d'`approximate`.
- **10.** Un commentaire dans `read_new` signale qu'un log tronqué puis re-rempli sur le même inode n'est pas détecté.
- **11.** `--end-of-options` passe désormais avant les révisions dans `diff`, `rev-list` et `Git::is_ancestor`. Le test `revisions_that_look_like_options_are_not_read_as_options` **échoue sans** l'option (git écrit le fichier désigné par `--output=`) et passe avec.
- **12.** `read_output(project_root, call)` canonicalise le chemin et refuse une cible hors de `.vibe/tool-output`. Un test symlink vérifie le refus.
- **13.** `from_toml` rejette les prix non finis ou négatifs. L'ordre de correspondance (clé exacte, puis `*/<model>` avec le fournisseur avant le premier `/`) est documenté dans `price_of` et `configuration.md`, avec des tests.
- **14.** `trace::pair_all_calls` fait un seul passage par tâche ; `Call.run` est ajouté. L'index est lu une fois pour tout l'historique.
- **15.** Un échec de `for-each-ref` est reporté dans les `errors` de chaque tâche. Je n'ai pas de test : je n'ai pas trouvé comment provoquer cet échec de façon fiable.
- **16.** Harmonisation des types de store et démarrage « à partir de maintenant » :
  - l'historique prend maintenant `&dyn PipelineStore`, comme `run_trace` ;
  - le trait `PipelineStore` gagne `task_numbers()` et `is_running()`, avec des valeurs par défaut vides/`false`, implémentées par `FileTaskStore` ;
  - `AllEventsFollower::from_end()` et `EventReader::at_end()` sont ajoutés et testés.
- **17.** `vibe_workspace` n'est importé que dans `history.rs`.

Vérification finale :
- fmt : sortie 0 ;
- clippy `-D warnings` : sortie 0, aucun avertissement (`Finished ... in 2.26s`) ;
- tests : 28 suites, 626 passés, 0 échec.
