# Rapport — Étape 1 : événements et persistance

L'étape 1 est terminée dans l'arbre de travail, sur `feat/visibility-0.5`. Rien n'est commité ni poussé. Le formatage, clippy et les tests passent : 596 tests en 27 suites, aucun échec. `TODOS.md` était déjà modifié avant mon passage et je n'y ai pas touché, pas plus qu'aux autres fichiers exclus.

## Fichiers modifiés

### vibe-core
- `crates/vibe-core/src/ids.rs` : nouveau type `CallId`, 48 bits aléatoires sérialisés en chaîne de 12 chiffres hexadécimaux.
  - Sa valeur par défaut est `nil()` (`000000000000`) ; il fournit aussi `is_nil()`, `parse`, `Display` et `FromStr`.
  - Test de sérialisation `call_id_serialises_as_short_hex_string`.
- `crates/vibe-core/src/lib.rs` : `CallId` est réexporté.
- `crates/vibe-core/src/event.rs` :
  - `AgentStarted` gagne `model: String` ;
  - `ToolCalled` gagne `call` et `subtask` ;
  - `ToolReturned` gagne `call`, `subtask`, `exit_code`, `timed_out`, `output_chars` et `output_file` ;
  - `RunFinished` gagne `usage`, `active_ms` et `started_at` ;
  - tous ces champs sont en `#[serde(default)]` ;
  - nouveaux événements `Committed { run, subtask, commit, message, files }` et `Merged { run, commit, branch, base }`, ajoutés à `TYPES`, `type_name()` et `run_id()` ;
  - tests `schema_two_logs_without_the_new_fields_still_read` et `new_events_roundtrip`.
- `crates/vibe-core/src/config.rs` :
  - `PipelineConfig.trace_outputs: bool` (défaut `true`) et `trace_max_chars: usize` (défaut `100_000`) ;
  - constantes `TOOL_OUTPUT_DIR = "tool-output"` et `DEFAULT_TRACE_MAX_CHARS`.
- `crates/vibe-core/src/task.rs` : `Task.branch: Option<String>`, en `#[serde(default, skip_serializing_if = "Option::is_none")]`.

### vibe-agents
- `crates/vibe-agents/src/runtime.rs` :
  - nouvelle struct publique `ToolTrace { dir: PathBuf, reference: String, max_chars: usize }` et méthode de construction `AgentRunner::tool_trace(trace)` ;
  - `execute_one` génère un `CallId` avant l'appel et le reprend dans `ToolReturned`, donc les appels parallèles s'apparient ;
  - `subtask` vient de `self.subtask` ;
  - `exit_code` et `timed_out` sont lus dans `ToolOutput.metadata` (cas de `bash`) ;
  - `output_chars` est la longueur en caractères avant toute troncature ;
  - le fichier de trace `<dir>/<call>.txt` est écrit avant la troncature destinée au modèle. Au-delà de `max_chars`, il est coupé et se termine par la ligne `[vibe: output cut to the first N of M characters]` ;
  - `output_file` vaut `<reference>/<call>.txt`. Un échec d'écriture produit un `tracing::warn` et `output_file: None` ;
  - la copie `<workspace_root>/.vibe/tool-output/<uuid>.txt` destinée au modèle (au-delà de 100 000 caractères) est inchangée ;
  - `AgentStarted.model` reprend le modèle du runner, ou `provider.info().default_model` quand il est vide.
- `crates/vibe-agents/src/lib.rs` : export de `ToolTrace` et paragraphe de doc du module.
- `crates/vibe-agents/tests/agent_loop.rs` : trois tests.
  - `parallel_tool_calls_pair_by_call_id_and_are_traced` : trois appels, dont le rapide revient avant le lent. Il vérifie l'appariement par id, `subtask`, `model`, les fichiers écrits, la limite de 20 caractères avec marqueur, et `exit_code = 3` / `timed_out = true` repris des métadonnées.
  - `without_a_trace_no_output_is_written`.
  - `traced_outputs_are_complete_while_the_model_sees_a_truncated_copy`.

### vibe-pipeline
- `crates/vibe-pipeline/src/store.rs` : `PipelineStore::task_dir_name(id) -> Result<Option<String>>`. L'implémentation par défaut renvoie `None` ; `FileTaskStore` renvoie `NNN-slug`.
- `crates/vibe-pipeline/src/pipeline.rs` :
  - `Pipeline::tool_trace(task, run)` construit `ToolTrace` quand `trace_outputs` est vrai :
    - dir : `<project_root>/.vibe/tool-output/<task dir>/<run>` ;
    - reference : `.vibe/tool-output/<task dir>/<run>` ;
  - il ne fait jamais échouer un run : sans nom de répertoire, ou si la lecture du nom échoue, l'id de tâche sert de nom ;
  - après `workspace.open`, `task.branch = workspace.branch` est enregistré si besoin. Un échec de sauvegarde produit un événement `log` de niveau `warn` et le run continue ;
  - le helper `run_finished(&RunState, status)` remplit `usage`, `active_ms` et `started_at`. Il sert à la fin normale (succès, échec, annulation, pause) et à l'échec de `workspace.open`.
- `crates/vibe-pipeline/src/context.rs` :
  - `RunContext.tool_trace: Option<ToolTrace>`, appliqué dans `runner_in` ;
  - `commit(message, subtask: Option<SubtaskId>)` publie `Committed` quand le committer renvoie un SHA ;
  - `committed(root, subtask, commit, message)` est public ;
  - `commit_files(root, sha)` est public et lance `git diff --name-only -z --no-renames <sha>^1 <sha>`, avec repli sur `git show` pour un commit racine. La liste est vide hors de git ;
  - test `commit_files_lists_the_files_of_a_commit` sur un vrai dépôt.
- `crates/vibe-pipeline/src/phases/build.rs` :
  - le checkpoint avant le build a `subtask: None` ;
  - un commit non isolé porte l'id de sous-tâche s'il n'en contient qu'une, sinon `None` (via `to_commit_ids`) ;
  - `integrate_attempt` publie `Committed` avec `subtask` avant `SubtaskIntegrated`, quand l'intégration produit un commit.
- `crates/vibe-pipeline/src/phases/fix.rs` : `ctx.commit(&msg, None)`.
- `crates/vibe-pipeline/src/phases/merge.rs` : publie `Merged { commit, branch: workspace.branch, base: workspace.base_branch }` pour `MergeOutcome::Merged { commit: Some(_) }`.
- `crates/vibe-pipeline/tests/pipeline.rs` :
  - `trivial_task_skips_spec` étendu : totaux de `RunFinished` égaux à `run.json`, `Committed` avec sa sous-tâche, `model` renseigné, `branch` à `None` en `in_place` ;
  - `isolated_subtasks_integrate_one_at_a_time_and_retry_conflicts` étendu : `Committed` du checkpoint, puis un par intégration avec son id ;
  - nouveaux tests :
    - `auto_merge_publishes_merged_and_records_the_branch` ;
    - `auto_merge_conflicts_publish_no_merged_event` ;
    - `tool_outputs_are_traced_under_the_task_directory` ;
    - `trace_outputs_off_writes_nothing`.

### vibe-cli
- `crates/vibe-cli/src/app.rs` : `worktree_location` utilise d'abord `task.branch`. Le répertoire est la branche sans le préfixe `vibe/`, sinon il est recalculé comme avant. Test `worktree_location_prefers_the_recorded_branch`.
- `crates/vibe-cli/src/commands/task.rs` : `discard` supprime `.vibe/tool-output/<task dir>/`. Le nom est lu dans l'index avant `delete_task`, et un répertoire absent est ignoré. `init.rs` ignorait déjà `tool-output/`, rien n'a changé là.
- `crates/vibe-cli/src/render.rs` : rendu de `committed` (en mode verbeux seulement) et de `merged`.
- `crates/vibe-cli/src/util.rs` : helper `short_sha`.
- `crates/vibe-cli/tests/cli.rs` : `dry_run_with_mock_then_resume_then_discard` vérifie que la branche est enregistrée dans `task.json` et que `discard` supprime le répertoire de trace.

### Docs
- `docs/src/reference/events.md` : lignes du tableau mises à jour, plus quatre sections : « Tool calls », « Trace store », « Commits », « Logs recorded before 0.5 ».
- `docs/src/user/configuration.md` : `trace_outputs` et `trace_max_chars` dans la table `[pipeline]`.
- `docs/src/design/adr/007-one-seam-many-interfaces.md` : une conséquence ajoutée sur le stockage des traces, `committed`/`merged` et les totaux de `run_finished`.
- `docs/src/design/persistence.md` : `tool-output/<task dir>/<run>/<call>.txt` dans l'arborescence, paragraphe sur le stockage, ligne de la table « What to commit », `branch` dans l'exemple de `task.json`.
- `docs/src/design/agent-runtime.md` : section « Trace of tool outputs ».
- `docs/src/design/domain-model.md` : ligne `branch` dans la table de `Task`.

## Formes des événements (`docs/src/reference/events.md`)

```
| `agent_started` | `run`, `role`, `subtask`, `model` | an agent session starts; `model` is the provider's model id |
| `tool_called` | `run`, `role`, `tool`, `input` (complete arguments), `call`, `subtask` | before a tool runs |
| `tool_returned` | `run`, `role`, `tool`, `is_error`, `duration_ms`, `preview` (first 200 characters), `call`, `subtask`, `exit_code`, `timed_out`, `output_chars`, `output_file` | after a tool ran |
| `committed` | `run`, `subtask`, `commit`, `message`, `files` | the pipeline committed in the task workspace: checkpoint before the build, subtask or fix commit, subtask integration |
| `merged` | `run`, `commit`, `branch`, `base` | the merge phase merged the task branch into its base (`auto_merge`); `commit` is the resulting commit of `base` |
| `run_finished` | `run`, `success`, `status`, `usage`, `active_ms`, `started_at` | the run ends, always last, also after `paused`; totals of the whole run, resumes included |
```

Le schéma reste `2`. Les anciens journaux se lisent avec les valeurs par défaut :
- `call` = `000000000000` ; ces appels s'apparient par ordre et par nom d'outil ;
- `subtask`, `exit_code` et `output_file` = `null` ;
- `timed_out` = `false` et `output_chars` = `0` ;
- `model` = `""` ;
- `usage` à zéro, `active_ms` = `0` et `started_at` = epoch.

## Écarts par rapport au plan

1. **Sorties volumineuses stockées deux fois.** Le plan disait que la copie `<uuid>.txt` du worktree « déménage » vers le stockage des traces. J'ai suivi ta consigne de ne pas la casser : les sorties de plus de 100 000 caractères existent en double, dans la trace et dans le workspace. En `in_place`, les deux copies sont sous `.vibe/tool-output/` du projet, dans des sous-chemins distincts.
2. **`vibe-pipeline` lance maintenant `git` lui-même** (`commit_files`) pour remplir `Committed.files`.
   - Jusque-là, ce crate ne lançait aucun processus : git passait par le `Committer` injecté. Le helper n'utilise pas non plus `vibe_workspace::Git`.
   - Je le juge acceptable : c'est un diff `--name-only` en lecture seule, qui n'exécute ni hook ni driver de diff, et qui renvoie une liste vide hors de git (committer factice des tests).
   - Alternative plus propre si tu préfères : injecter une fonction qui liste les fichiers d'un commit dans `PipelineDeps`.
3. **Deux commits restent sans événement `committed`** : le checkpoint `vibe: checkpoint` qu'un worktree fait avant le merge, et le commit de résolution de conflits assistée. Tous deux sont faits dans `vibe-workspace`, hors de portée du pipeline ; `events.md` le mentionne.
4. **`started_at` vaut l'epoch Unix** dans les anciens journaux. C'est une valeur sentinelle, documentée comme telle.
5. **Nouvelle méthode `PipelineStore::task_dir_name`**, avec une implémentation par défaut, donc sans casser les implémentations existantes. Les stores sans nom de répertoire nomment la trace d'après l'id de tâche.
6. **`Task.branch` ne fixe que la branche.** Le chemin du worktree en est dérivé (préfixe `vibe/` retiré) ; il n'est pas stocké.

## Vérification

`cargo fmt --all` a reformaté 4 fichiers (`crates/vibe-agents/tests/agent_loop.rs`, `crates/vibe-pipeline/tests/pipeline.rs`, `crates/vibe-pipeline/src/context.rs` et `crates/vibe-pipeline/src/pipeline.rs`). Ensuite :

- `cargo fmt --all -- --check` : code de sortie 0, sans sortie.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features`, dernières lignes :
  ```
      Checking vibe-pipeline v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-pipeline)
      Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-cli)
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.51s
  ```
- `cargo test --workspace --all-features` : code de sortie 0. Les 27 lignes `test result` sont toutes `ok`, pour 596 tests passés au total ; aucun échec ni panic. Dernière ligne :
  ```
  test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  ```
