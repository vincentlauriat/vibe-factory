# Inventaire : données et vues existantes (historique, flux global, trace de run)

Lecture seule, sans propositions. Les références sont des chemins `fichier:ligne` relatifs à la racine du dépôt.

## 1. Persistance sous `.vibe/`

- **Arborescence.** Elle est documentée en `crates/vibe-pipeline/src/store.rs:4-16`, avec les constantes aux lignes 38-54.
  - `tasks/index.json` : `Index { next_number, tasks: BTreeMap<TaskId, IndexEntry{dir, number}> }` (`store.rs:172-186`).
  - Dans chaque `tasks/NNN-slug/` :
    - `task.json`, `spec.json`/`.md`, `plan.json`/`.md`, `qa_report_<round>.json`/`.md`.
    - `progress.md` : ajouts horodatés `### YYYY-MM-DD HH:MM:SS UTC` (`store.rs:501-514`).
    - `memory/gotchas.md` et `memory/patterns.md` : une ligne `- [date] note` par entrée (`store.rs:532-546`).
    - `events.jsonl` : **tous les runs** de la tâche.
    - `run.json` : **seulement le dernier run**.
    - `run.lock`, `run.owner`, `cancel.request`.
  - Fichiers globaux : `tasks/.index.lock`, `.vibe/memory.jsonl` (`crates/vibe-pipeline/src/memory.rs:17`), `.vibe/server.token` (`crates/vibe-cli/src/server/mod.rs:26`), `.vibe/worktrees/`, `.vibe/tool-output/`.
  - `.vibe/.gitignore` ignore `worktrees/` et `tool-output/`. `tasks/` est **commité volontairement** (`crates/vibe-cli/src/commands/init.rs:12-15`).
- **`Task`** (`crates/vibe-core/src/task.rs:93-114`) : `id, title, description, status, complexity, labels, source, created_at, updated_at`.
  - Pas de branche, pas de commit, pas d'usage, pas de `finished_at`.
  - `TaskStatus` : `Backlog/Planning/Building/Review/Ready/Done/Failed/Cancelled`.
- **`RunState`**, écrit dans `run.json` (`crates/vibe-pipeline/src/state.rs:77-136`) :
  - identité et phases : `run_id, task_id, current_phase, completed_phases, qa_round, profile` ;
  - temps et statut : `started_at, updated_at, status` (`Running/Paused/Finished/Failed/Cancelled`) ;
  - validations : `validations: Vec<ValidationResult>`, `validation_fix_attempts`, `pending_validation_fix` ;
  - coût : `usage: Usage` et `active_ms` (tous deux cumulés sur les reprises) ;
  - approbations : `pending_approval, approved_gates, rejection, approvals: Vec<ApprovalRecord{gate, approved, comment, at}>`, `pending_human_fix` ;
  - erreur : `last_error`.
  - **Absents** : `finished_at` (seul `updated_at` existe), branche, workspace, commit de merge, fichiers modifiés, usage par phase.
  - `run.json` est écrasé à chaque nouveau run. L'historique des runs précédents n'existe que dans `events.jsonl`.
- **`ValidationResult`** (`state.rs:42-59`) : `integration, workspace_root, command, finished_at, passed, output` (sortie complète), `metadata` (exit_code, timeout, elapsed).
- **`Usage`** (`crates/vibe-core/src/provider.rs:73-84`) : `input_tokens, output_tokens, cache_read_tokens, cache_write_tokens`.
- **`Plan` / `Subtask`** (`crates/vibe-core/src/plan.rs:26-94`) : `Subtask{id, title, description, files` (fichiers **prévus** par le planner), `depends_on, verification, status, attempts, notes}`. Pas de commit par sous-tâche.
- **`QaReport`** (`crates/vibe-core/src/qa.rs:58-70`) : `task_id, round, verdict, summary, issues[{severity, title, detail, requirement, file, line, suggested_fix}]`. Pas d'horodatage.
- **`Workspace`** (`crates/vibe-core/src/workspace.rs:23-36`) : `root, project_root, kind, branch, base_branch`. **Jamais persisté** : il est reconstruit à chaque run par `workspace.open` (`crates/vibe-pipeline/src/pipeline.rs:461`).

## 2. Événements

`crates/vibe-core/src/event.rs:15-219` définit 20 variantes, listées dans `TYPES` (l. 223-244). Toutes portent `run: RunId` ; seul `Log` a un `run` optionnel.

| Événement | Champs |
|---|---|
| `RunStarted` | `run, task` — **seul événement portant le `task`** |
| `PhaseStarted` | `run, phase` |
| `PhaseFinished` | `run, phase, success, summary` — sans usage ni durée |
| `AgentStarted` | `run, role, subtask: Option` — sans nom de modèle |
| `AgentText` | `run, role, text` — texte complet d'un step ; **sans subtask** |
| `AgentDelta` | `run, role, subtask, delta: StreamDelta{kind: text\|thinking, text}` — **éphémère** |
| `ToolCalled` | `run, role, tool, input` — **arguments complets** ; **ni id d'appel, ni subtask** |
| `ToolReturned` | `run, role, tool, is_error, duration_ms, preview` |
| `AgentFinished` | `run, role, steps, usage, stop` (`AgentStop` sérialisé en chaîne) |
| `SubtaskUpdated` | `run, subtask, status` |
| `ValidationFinished` | `run, command, integration, passed, exit_code` — sans la sortie (elle est dans `run.json`) |
| `SubtaskIntegrated` | `run, subtask, commit: Option<String>, conflicts: Vec<String>` |
| `BudgetUpdated` | `run, tokens` (total, sans détail in/out), `token_limit, active_ms, duration_limit_ms` |
| `ArtefactWritten` | `run, artefact{spec\|plan\|qa_report{round}}` |
| `ApprovalRequested` | `run, gate` |
| `ApprovalResolved` | `run, gate, approved, comment` |
| `Retrying` | `run, what, attempt, delay_ms` |
| `Paused` | `run, reason` |
| `RunFinished` | `run, success, status` |
| `Log` | `run?, level, message` |

- **Troncature des sorties d'outil** (`crates/vibe-agents/src/runtime.rs:718-801`) :
  - `preview = truncate_chars(output.content, PREVIEW_CHARS)`, avec `PREVIEW_CHARS = 200` (`runtime.rs:63`, fonction à la l. 875).
  - Le contenu renvoyé au modèle est lui-même tronqué à `DEFAULT_MAX_TOOL_OUTPUT_CHARS = 100_000` (`runtime.rs:27`). Au-delà, la sortie complète va dans `<workspace_root>/.vibe/tool-output/<uuid>.txt` (`runtime.rs:806-829`, 882-887), donc dans le worktree de la tâche.
  - **`ToolOutput.metadata`** (`crates/vibe-core/src/tool.rs:183-192`) **n'est pas transmis dans l'événement.** Pour `bash`, il contient `exit_code, timed_out, duration_ms, runner, sandbox_error, denied` (`crates/vibe-tools/src/tools/bash.rs:451-460`).
  - **La sortie complète d'un outil n'est persistée nulle part** en dessous de 100 000 caractères : elle ne vit que dans la conversation du modèle.
- **Appariement `ToolCalled`/`ToolReturned`** : seulement par ordre et par `tool`. Les outils non mutants s'exécutent en parallèle via `join_all` (`runtime.rs:696-712`), donc l'ordre des retours n'est pas garanti.
- **Noms des outils intégrés** (`crates/vibe-tools/src/tools/*.rs`) : `bash`, `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `web_fetch`, `web_search`.
- **Éphémères** : `is_ephemeral()` ne vaut vrai que pour `AgentDelta` (`event.rs:320-322`). Les `Log` sans run n'ont pas de `seq` et sont ignorés par `FileEventSink::for_run`.
- **Commits et fichiers** :
  - `SubtaskIntegrated.commit` est **le seul événement qui porte un hash** (publié en `crates/vibe-pipeline/src/phases/build.rs:373`), et il ne liste pas les fichiers modifiés (seulement les conflits).
  - Les commits de checkpoint (`ctx.commit` en `crates/vibe-pipeline/src/context.rs:488-501`, appelé depuis `build.rs:517`, `build.rs:729` et `phases/fix.rs:197`) renvoient un SHA **ignoré par les appelants, sans événement**.
  - Le merge final (`crates/vibe-pipeline/src/phases/merge.rs:60-66`) ne met le SHA que dans `progress.md` et dans le `PhaseFinished.summary` (« merged (sha) »). Il n'y a pas d'événement dédié.
  - **Aucun événement « fichier écrit »** : il faut lire `ToolCalled{tool: "write_file"|"edit_file", input}`.
  - **Aucun événement ne porte le nom du modèle ni l'usage par appel LLM** : l'usage est agrégé par session (`AgentFinished`) et par run (`BudgetUpdated`). `PhaseResult.usage` (`context.rs:196`) n'apparaît que dans `RunReport`, donc dans la sortie `vibe run --json`, et n'est pas persisté.
- **Enveloppe** (`event.rs:335-348`) : `schema` (= 2, `EVENT_SCHEMA_VERSION` l. 327), `seq` (par run, sans trou, continué sur les reprises via `resume_sequence`, `pipeline.rs:442-450`), `at`, `event`.

## 3. Écriture et lecture du journal

- `EventBus` (`event.rs:372-465`) : un canal `broadcast` de capacité 1024, plus des sinks attendus à chaque publication ; un mutex d'ordre garantit la livraison des `seq` dans l'ordre.
- `Pipeline::execute` installe une fois un `EventRouter` (`pipeline.rs:108-142`, 431-440) qui route chaque run vers `store.event_sink(task, run)`, c'est-à-dire un `FileEventSink::for_run` qui ajoute à `events.jsonl` (`store.rs:560-563`, 623-677).
- `load_events` relit et parse **tout le fichier** (`store.rs:596-615`) : pas d'index, pas de lecture partielle.
- `vibe approve`/`vibe reject` écrivent `ApprovalResolved` **hors processus**, en calculant `seq = max + 1` (`crates/vibe-cli/src/commands/approval.rs:69-86`).
- **`vibe events <REF>`** (`crates/vibe-cli/src/commands/events.rs:23-66`, arguments en `crates/vibe-cli/src/cli.rs:311-324`) :
  - `REF` est **obligatoire** ;
  - `--after SEQ`, `-f/--follow` (relit tout le fichier toutes les 250 ms et s'arrête sur `RunFinished`), `--all` (tous les runs, incompatible avec `--follow`) ;
  - le rendu passe par `Renderer` ; avec `--json` global, chaque enveloppe est imprimée brute (`crates/vibe-cli/src/render.rs:287-299`).
  - **Rien ne lit les événements de toutes les tâches** : aucune API de store, commande ou route ne fait cela.
- **`RunManager::subscribe`** (`crates/vibe-pipeline/src/manager.rs:74-78`) : il diffuse les événements de **tous les runs de ce processus**, toutes tâches confondues, puisque le bus est unique par pipeline. Les runs d'autres processus n'y apparaissent pas.
- **Côté serveur**, les autres processus sont captés par sondage du fichier : `stream` relit `run.json` et tout `events.jsonl` toutes les 300 ms (`crates/vibe-cli/src/server/api.rs:24`, 414-465). Le bus en processus ne sert qu'à relayer les `AgentDelta`.
- **Rattachement événement → tâche** : hors `RunStarted`, un événement ne porte que `run` ; la tâche se déduit du fichier `events.jsonl` d'où il provient (ou du `run.json`).

## 4. API du serveur

Routes définies en `crates/vibe-cli/src/server/api.rs:84-104`, toutes sous `/api`, avec authentification Bearer. Seules les routes `/stream` acceptent `?token=` (l. 141). Un contrôle `Host` protège du DNS rebinding (l. 108-120).

| Méthode et chemin | Réponse |
|---|---|
| `GET /` | `index.html` embarqué |
| `GET /api/health` | `{name, version}` |
| `GET /api/tasks` | tableau de `row` : `{task, number, running, run: {status, current_phase, pending_approval}}` (l. 178-198) |
| `POST /api/tasks` | corps `{title, description}` ; renvoie 201 et un `row` |
| `GET /api/tasks/{task}` | le `row` avec `run` = `RunState` complet, plus `spec, plan, qa_reports` (l. 219-232) |
| `POST /api/tasks/{task}/run` | corps `{resume}` ; renvoie 202 |
| `POST /api/tasks/{task}/cancel` | annulation (manager, sinon `cancel.request`) |
| `POST /api/tasks/{task}/approve` | corps `{comment}` |
| `POST /api/tasks/{task}/reject` | corps `{reason}` |
| `GET /api/tasks/{task}/changes` | `{changes: String}` |
| `GET /api/tasks/{task}/events?after=&all=` | JSON `Vec<Envelope>` (l. 369-399) |
| `GET /api/tasks/{task}/stream?after=&token=` | SSE : nom = `type_name`, id = `seq`, data = enveloppe. Dernier run seulement ; un nouveau run repart de 0 (l. 401-477) |
| `GET /api/evals` | fichiers `summary.json` trouvés sous `--evals` (profondeur 4) |

- **Aucune route globale d'événements ni de SSE multi-tâches.** Aucun filtre par type d'événement.
- **Calcul des « changes »** : `tui::changes` (`crates/vibe-cli/src/tui/mod.rs:210-221`).
  - Avec worktree ou container : `GitWorktreeProvider::changes` (`crates/vibe-workspace/src/worktree.rs:613-633`) exécute `git diff --stat <base>...HEAD -- . :(exclude).vibe` (`crates/vibe-workspace/src/git.rs:314-318`) puis `git status --short` **dans le worktree**.
  - En `in_place` : `git status --short` à la racine du projet.
  - Le résultat est un **stat textuel, pas un diff complet**. `Git::diff` existe (`git.rs:323`) mais n'est pas exposé.
  - Si le worktree est supprimé ou la tâche mergée, le résultat vaut « The task has no workspace yet. ».

## 5. Interface web

- Un seul fichier, `crates/vibe-cli/src/server/web/index.html` (~20 Ko, JS vanilla).
- Deux vues, **Tasks** et **Evaluations** (l. 83-84, `showView` l. 352).
- La vue Tasks est un board latéral rafraîchi toutes les 2 s (`GET /tasks`), plus le détail de la tâche sélectionnée (rafraîchi toutes les 3 s si elle tourne) :
  - boutons Run / Resume / Cancel, bannière d'approbation, barres de budget alimentées par `budget_updated` ;
  - onglets `activity`, `plan`, `spec`, `qa`, `changes` (l. 238).
- **SSE** : **un `EventSource` par tâche sélectionnée**, fermé au changement de sélection (`openStream`, l. 288-312). Il écoute les 20 types. `agent_delta` s'accumule dans `state.live`, limité aux 2000 derniers caractères.
- **Onglet Activity** (`describe`, l. 164-184 ; `renderTab`, l. 248-252) : il garde les 400 dernières lignes.
  - `agent_text` est tronqué à 240 caractères ;
  - `tool_called` affiche `JSON.stringify(input).slice(0,160)` ;
  - `tool_returned` n'est **affiché qu'en cas d'erreur** (aperçu tronqué à 160) ;
  - sont masqués : `subtask_integrated` sans conflit, `log` de niveau info, et `budget_updated` (qui ne sert qu'aux barres).
- `refreshDetail` est déclenché par `artefact_written`, `subtask_updated`, `approval_*`, `run_finished` et `paused`.

## 6. Interface terminal (TUI)

- Structure dans `crates/vibe-cli/src/tui/` : `mod.rs` (boucle), `app.rs` (état et touches), `view.rs` (rendu).
- Board des tâches et détail de la tâche sélectionnée avec trois onglets `Activity / Plan / Changes` (`app.rs:101-131`), plus des jauges de budget (`view.rs:177`).
- **Sources d'événements** :
  - toutes les 500 ms (`TICK`), relecture de `list_tasks`, du `run.json` et de **tout le `events.jsonl` de la tâche sélectionnée** (`mod.rs:230-262`), puis application incrémentale via `consumed` ;
  - en direct, seuls les `AgentDelta` du run sélectionné sont utilisés (`app.rs:381-399`), limités à `STREAM_LIMIT = 4000` caractères.
- **Activité** : au plus `ACTIVITY_LIMIT = 500` lignes (`app.rs:19`). `describe` (`app.rs:443-560`) :
  - `AgentText` est tronqué à 300 ;
  - `ToolCalled` affiche le JSON des arguments tronqué à 120 ;
  - `ToolReturned` n'apparaît qu'en erreur (aperçu à 120) ;
  - sont masqués : `SubtaskIntegrated` sans conflit, `Log` info, `BudgetUpdated`, `AgentDelta`.
- **Touches** (`app.rs:266-352`) : `q`/`Esc` quitter, `j`/`k`/flèches naviguer, `Tab` changer d'onglet (charge les changes), `n` nouvelle tâche, `r` lancer, `R` reprendre, `c` annuler, `a` approuver, `x` rejeter (raison obligatoire), `g` rafraîchir, `Ctrl-C`.
- Les runs lancés par la TUI sont annulés à la sortie (restent reprenables) (`mod.rs:120-126`).

## 7. Ligne de commande et coûts

- **`vibe task list`** (`crates/vibe-cli/src/commands/task.rs:97-142`) : le JSON est `[{number, task}]`. Le tableau affiche `# / status / complexity / title / updated`. Done et Cancelled sont masqués sauf avec `--all` ; filtre possible par `--status`.
- **`vibe task show --json`** (l. 163-175) : `{number, task, dir, spec, plan, qa_reports, run: RunState, worktree: {branch, path, branch_exists, dir_exists}}`.
- **`vibe run --json`** : résumé `summary_json` (`crates/vibe-cli/src/render.rs:419-438`) avec `usage` (cette invocation), `run_usage`, `run_active_ms`, `duration_ms`, `phases` (`PhaseResult` avec usage par phase), `branch, worktree, task_dir`. `summary_text` affiche les tokens in/out par phase.
- **`vibe status --json`** : `by_status`, `recent_runs`, `worktrees` (`crates/vibe-cli/src/commands/status.rs:34-50`).
- **Aucun calcul de prix ou de coût** dans le code Rust (aucune occurrence de `price` ou `cost` dans le domaine). Seul `evals/run.py:139` contient `"estimated_cost": None`. Aucun tarif par modèle dans la configuration.

## 8. Workspace et intégration

- **Nommage** :
  - branche de tâche `vibe/<slug>-<short id>`, répertoire `.vibe/worktrees/<slug>-<short id>` (`crates/vibe-workspace/src/worktree.rs:19`, 82-100) ;
  - tentative de sous-tâche : branche `<task_branch>--<label>` (ex. `s2-a1`) et répertoire frère `<name>--<label>` (`crates/vibe-workspace/src/subtask.rs:30`, 80-93) ;
  - la base est mémorisée en config git `branch.<b>.vibebase` (`worktree.rs:24`).
- **Commits** :
  - `commit_all` utilise l'identité « Vibe Factory » et exclut `.vibe` (`crates/vibe-workspace/src/commit.rs:15-41`) ;
  - messages : « vibe: checkpoint before build », `commit_message` des sous-tâches (`build.rs:246`), un commit de fix (`fix.rs:197`), « vibe: checkpoint » avant le merge (`worktree.rs:16`) ;
  - intégration des sous-tâches : `--ff-only` sinon `--no-ff` dans la branche de tâche (`subtask.rs:165-235`).
- **Merge final** (`crates/vibe-pipeline/src/phases/merge.rs:23-110`) :
  - avec `auto_merge = false` (défaut forcé par `vibe serve`, `server/mod.rs:60`) : statut `Ready`, rien n'est mergé ;
  - sinon : `merge` ou `merge_validated`, ff ou « Merge <branch> (vibe) », puis `Done`.
  - **Le SHA n'est gardé que dans `progress.md` et `PhaseFinished.summary`.**
  - **Aucune commande ne passe une tâche `Ready` à `Done`** après un merge manuel ou une PR mergée : `TaskStatus::Done` n'est posé que par la pipeline.
- **Nom de branche** : il n'est **pas stocké** dans le record de tâche. `vibe pr` (`crates/vibe-cli/src/commands/pr.rs:31-45`), `task show`, `task discard` et `tui::changes` le **recalculent** via `worktree_location` (`crates/vibe-cli/src/app.rs:443-452`), qui utilise toujours `GitWorktreeProvider::new()`, donc le répertoire par défaut `.vibe/worktrees`.
  - Base de `vibe pr` : `--base`, sinon `branch.<b>.vibebase`, sinon `config.base_branch`, sinon la branche par défaut.
  - `vibe pr` ajoute l'URL de la PR à `progress.md` seulement.
- **`task discard`** supprime le worktree, la branche et le répertoire de la tâche (`task.rs:311-377`). L'historique git reste la seule trace.

## 9. Contraintes (ADR-007 et tests)

- **Règle ADR-007** (`docs/src/design/adr/007-one-seam-many-interfaces.md`) :
  - un seul point d'entrée, le `RunManager`, qui démarre, reprend, annule, liste et fournit la souscription, et ne rend rien ;
  - **les événements sont le contrat** : chaque changement visible est un événement numéroté par run, versionné et persisté dans `events.jsonl`, et les interfaces ne gardent aucune vue privée que les événements ne sauraient reconstruire ;
  - `.vibe/` reste la source de vérité, protégée par verrou inter-processus ;
  - le schéma d'événements est une API publique : ajouter un type ou un champ optionnel est non cassant, supprimer ou modifier un champ incrémente `schema` (`docs/src/reference/events.md`).
- **Tests de cohérence** :
  - `every_event_type_is_documented` (`crates/vibe-core/src/event.rs:571-583`) vérifie que chaque nom de `Event::TYPES` apparaît sous la forme `` | `name` | `` dans `docs/src/reference/events.md` ;
  - `type_names_match_serde_tags` (`event.rs:550-568`) ;
  - `logs_without_version_read_as_version_one` (`event.rs:536-547`) ;
  - `numbering_skips_ephemeral_events_and_resumes` (`event.rs:498-533`).
  - **Attention** : `TYPES` et `type_name()` sont des listes manuelles, à tenir à jour en même temps que l'enum.
