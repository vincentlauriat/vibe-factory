# Rapport — Étape 2 : couche de lecture

L'étape 2 est terminée dans l'arbre de travail, sur `feat/visibility-0.5`. Rien n'est commité ni poussé.
- Formatage, clippy (`-D warnings`) et tests passent : **615 tests en 28 suites, 0 échec**, contre 596 en 27 suites à l'étape 1. J'ai ajouté 16 tests, qui ne comblent pas tout l'écart de 19 : je n'ai pas cherché d'où viennent les 3 autres.
- Je n'ai touché ni à `PLAN.md`, `TODOS.md`, `CHANGELOG.md`, `README.md`, `COMMANDS.md`, `CHANGES.md`, ni à aucune CLI, serveur ou TUI.
- `git status` montre aussi `TODOS.md` modifié et un répertoire `apps/` non suivi. Ce n'est pas de mon fait, je n'y ai pas touché.

## Fichiers modifiés

### vibe-core
- `crates/vibe-core/src/config.rs`
  - `VibeConfig.pricing: BTreeMap<String, ModelPrice>` : `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`, vide par défaut.
  - `ModelPrice { input: f64, output: f64, cache_read: Option<f64>, cache_write: Option<f64> }` : `deny_unknown_fields`, en USD par million de tokens.
  - `ModelPrice::cost(usage) -> Option<f64>` renvoie `None` quand des tokens de cache sont présents sans prix correspondant.
  - `VibeConfig::price_of(model) -> Option<ModelPrice>` : voir l'écart 3.
  - Test `pricing_by_model_id`.
- `crates/vibe-core/src/lib.rs` : réexport de `ModelPrice`.

### vibe-pipeline
- `Cargo.toml` : dépendance à `vibe-workspace`, déjà dans `[workspace.dependencies]` (écart 1). `Cargo.lock` : une ligne en plus.
- `src/lib.rs` :
  - modules `events_log`, `history`, `trace` ;
  - section « Read layer » dans la doc du crate ;
  - phrase sur les dépendances corrigée ;
  - réexports.
- `src/store.rs` :
  - `FileTaskStore::entries() -> Result<BTreeMap<TaskId, IndexEntry>>` (public) ;
  - `FileTaskStore::at_root(PathBuf)` (`pub(crate)`, sans création de répertoire, pour le follower).
- `src/events_log.rs` (nouveau) : `EventReader`, `TaggedEnvelope`, `FileTaskStore::all_events`, `AllEventsFollower`, plus 5 tests unitaires.
- `src/trace.rs` (nouveau) : `Call`, `PairedBy`, `RunTrace`, `pair_calls`, `files_written`, `last_run`, `run_trace`, `read_output`, `trace_file_path`, plus 4 tests unitaires.
- `src/history.rs` (nouveau) : résumés de runs, fichiers modifiés, coût, `task_history`, `project_history`.
- `tests/history.rs` (nouveau), 6 tests :
  - reprise de run ;
  - ancien schéma avec repli sur `run.json` ;
  - coût ;
  - parsing `--name-status` ;
  - dépôt git temporaire couvrant les 5 sources de fichiers, le filtre et le tri ;
  - projet hors git.

### Docs
- `docs/src/user/configuration.md` : ligne `[pricing."<provider>/<model>"]` dans la table des clés de premier niveau, et nouvelle section `## [pricing]` (table, règle de correspondance, « pas de coût plutôt qu'une estimation »).
- `docs/src/design/persistence.md` : nouvelle section « Reading the store: the read layer » (table des API, lecture incrémentale, forme de `TaggedEnvelope`, runs et reprises, ordre des sources de fichiers modifiés, garde-fou sur les chemins de trace).

## API publiques pour les étapes suivantes

Tout est réexporté à la racine de `vibe_pipeline`, sauf mention `history::` / `trace::`.

### A. Lecture incrémentale (`events_log`)

```rust
pub struct EventReader { /* path, offset, pending, identity */ }
impl EventReader {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn path(&self) -> &Path;
    pub fn offset(&self) -> u64;
    pub async fn read_new(&mut self) -> Result<Vec<Envelope>>;
}
```

Comportement de `read_new` :
- il lit des octets et ne décode que jusqu'au dernier `\n`, pour ne jamais couper un caractère UTF-8. La ligne incomplète est gardée pour l'appel suivant ;
- les lignes complètes illisibles sont sautées, comme dans `parse_event_log` ;
- un fichier absent donne un résultat vide et remet la lecture à 0 ;
- un fichier plus court que l'offset (tronqué) fait repartir de 0 ;
- sous unix, un fichier remplacé (inode différent) fait aussi repartir de 0.

### B. Toutes les tâches

```rust
#[derive(Serialize, Deserialize)]
pub struct TaggedEnvelope { pub task: TaskId, pub number: u32, #[serde(flatten)] pub envelope: Envelope }
// JSON : {"task":"…","number":3,"schema":2,"seq":17,"at":"…","event":{…}}

impl FileTaskStore {
    pub async fn all_events(&self, since: Option<DateTime<Utc>>) -> Result<Vec<TaggedEnvelope>>;
}
pub struct AllEventsFollower { .. }
impl AllEventsFollower {
    pub fn new(store: &FileTaskStore, since: Option<DateTime<Utc>>) -> Self;
    pub async fn poll(&mut self) -> Result<Vec<TaggedEnvelope>>;
}
```

- Le filtre est `at > since`.
- Le tri est stable, par `at`, puis numéro de tâche, puis `seq`. J'ai préféré le numéro de tâche à l'UUID pour départager : c'est plus lisible.
- Le premier `poll` renvoie la même chose que `all_events(since)`, les suivants seulement les ajouts, triés de la même façon. Pour `--follow`, la CLI peut donc utiliser le follower seul, sans course entre lecture initiale et suivi.
- Le follower est `'static` : il copie la racine du store, il n'emprunte pas le store. Il peut donc vivre dans une tâche SSE.
- `index.json` est relu à chaque `poll` : les nouvelles tâches sont suivies depuis leur début, les tâches supprimées sont abandonnées.

### C. Historique (`history`)

```rust
pub fn summarize_runs(events: &[Envelope]) -> Vec<RunSummary>;               // pur, testable
impl RunSummary { pub fn apply_run_state(&mut self, state: &RunState); }
pub fn parse_name_status(output: &str) -> Vec<FileChange>;
pub fn cost_of(events: &[Envelope], config: &VibeConfig) -> Option<Cost>;

pub async fn task_history(store: &FileTaskStore, project_root: &Path, config: &VibeConfig,
                          task: Task, branch: Option<&str>) -> Result<TaskHistory>;
pub async fn project_history(store: &FileTaskStore, project_root: &Path, config: &VibeConfig,
                             filter: HistoryFilter,
                             branch_of: &(dyn Fn(&Task) -> Option<String> + Sync))
                             -> Result<Vec<TaskHistory>>;
pub struct HistoryFilter { pub all: bool }   // Default : Ready/Done ; all : + Failed/Cancelled
```

Types (tous `Serialize`/`Deserialize`) :

- **`RunSummary`**
  - `run`, `started_at`, `finished_at: Option`, `status: Option<TaskStatus>`, `success: Option<bool>`, `resumes: u32` ;
  - `usage`, `active_ms`, `totals_known: bool` ;
  - `phases: Vec<PhaseSummary{phase, started_at, finished_at, success, summary}>` ;
  - `commits: Vec<CommitRecord{commit, message, files, subtask, at}>` ;
  - `merged: Option<MergeRecord{commit, branch, base, at}>` ;
  - `validations: Vec<ValidationRecord{command, integration, passed, exit_code, at}>` ;
  - `approvals: Vec<state::ApprovalRecord>`, `pending_approval: Option<ApprovalGate>` ;
  - `last_error: Option<String>`, `last_event_at`.
- **`ChangedFiles`** : `{ source: ChangedFilesSource, approximate: bool, files: Vec<FileChange{path, status, old_path}> }`.
  - `ChangedFilesSource`, tagué `kind` : `Branch{branch, base}`, `MergeCommit{commit, fast_forward}`, `Commits`, `Trace`, `None`.
  - `FileStatus` : `Added`, `Modified`, `Deleted`, `Renamed`, `Copied`, `Unknown`.
- **`TaskHistory`**
  - `task`, `number`, `runs` ;
  - `totals: HistoryTotals{usage, active_ms, runs, commits, complete}` ;
  - `changed_files` ;
  - `last_qa: Option<QaSummary{round, verdict, summary, issues}>` ;
  - `validations_passed: Option<bool>` ;
  - `cost: Option<Cost{amount, currency, complete}>` ;
  - `last_activity`, `errors: Vec<String>`.

Règles :
- **Regroupement des runs.** Les runs sont regroupés par `RunId`, car une reprise republie `run_started` avec le même id.
  - Les totaux viennent du dernier `run_finished`, jamais d'une somme.
  - Un `run_finished` dont `started_at` vaut l'epoch (ancien journal) laisse `totals_known = false`.
  - `apply_run_state` complète avec `run.json` si `run_id` correspond.
- **Erreurs.** `last_error` vient d'un `phase_finished` en échec (« `<phase> failed: <summary>` ») ou d'un `log` de niveau `error`. Il est effacé par un `run_finished` réussi et remplacé par `run.json.last_error` s'il existe.
- **Commits.** Un `subtask_integrated` d'ancien journal devient un `CommitRecord` sans message ni fichiers, dédupliqué par SHA avec `committed`.
- **Coûts git.**
  - Pour tout le projet : `rev-parse --is-inside-work-tree`, `for-each-ref refs/heads` et `config --local -z --get-regexp '^branch\..*\.vibebase$'`, plus `default_branch()` seulement si nécessaire, mis en cache.
  - Par tâche : un `git diff` pour une branche existante, ou `rev-list --parents` suivi d'un `diff` pour un merge.
  - Toutes ces commandes passent par `vibe_workspace::Git`, donc avec hooks, fsmonitor et pager neutralisés. S'y ajoutent `--no-ext-diff` et l'exclusion de `.vibe`.
- **Ordre des sources de fichiers modifiés :** `MergeCommit` (si un `merged` existe), puis `Branch` (branche existante, diff non vide), puis `Commits`, puis `Trace`, puis `None`. Voir l'écart 11.
- **Échecs git.** Une erreur git va dans `TaskHistory.errors` et la source suivante est essayée. Seules les erreurs du store font échouer l'historique.
- **Tri.** `last_activity` décroissant (max du dernier `at` et de `task.updated_at`), puis numéro décroissant.

### D. Trace (`trace`)

```rust
pub enum PairedBy { Id, Order, Unmatched }
pub struct Call { call, role, subtask, tool, input, called_at, returned_at: Option, duration_ms: Option<u64>,
                  is_error: Option<bool>, exit_code: Option<i64>, timed_out: bool, preview: String,
                  output_chars: u64, output_file: Option<PathBuf> /* absolu */, paired: PairedBy }
impl Call { pub fn written_path(&self) -> Option<&str>; }
pub struct RunTrace { pub run: RunId, pub calls: Vec<Call>, pub files_written: Vec<String> }
impl RunTrace { pub fn by_subtask(&self) -> BTreeMap<Option<SubtaskId>, Vec<&Call>>; }

pub fn pair_calls(events: &[Envelope], run: RunId, project_root: &Path) -> Vec<Call>;
pub fn files_written(calls: &[Call]) -> Vec<String>;
pub fn last_run(events: &[Envelope]) -> Option<RunId>;
pub fn trace_file_path(project_root: &Path, output_file: &str) -> Option<PathBuf>;
pub async fn run_trace(store: &dyn PipelineStore, project_root: &Path, task: TaskId,
                       run: Option<RunId>) -> Result<Option<RunTrace>>;
pub async fn read_output(call: &Call) -> Result<Option<String>>;
pub const WRITING_TOOLS: &[&str] = &["write_file", "edit_file"];
```

- **Appariement.** Les appels s'apparient par `call` quand il n'est pas nul, ce qui gère les retours dans le désordre. Sinon, ils s'apparient par file FIFO sur (role, tool, subtask) et sont marqués `Order`.
- **Appels orphelins.**
  - Un appel sans retour reste `Unmatched`, avec `returned_at = None`.
  - Un retour sans appel devient un `Call` `Unmatched` avec `input = null`, placé à l'heure de son retour.
- **Chemin de la trace.** `output_file` n'est gardé que s'il s'agit d'un chemin relatif sous `.vibe/tool-output/`, sans `..` ; il est alors résolu contre `project_root`. `read_output` renvoie `None` si le fichier n'existe plus.
- **Fichiers écrits.** `files_written` ne retient que les appels revenus sans erreur (`is_error == Some(false)`), sans doublons, dans l'ordre de la première écriture.

## Écarts par rapport au plan ou à la consigne

1. **`vibe-pipeline` dépend désormais de `vibe-workspace`.**
   - Le crate n'en dépendait pas, et `lib.rs` affirmait ne dépendre que de `vibe-core` et `vibe-agents`. Tu demandais de réutiliser les helpers de `vibe-workspace`.
   - Il n'y a pas de cycle : `vibe-workspace` ne dépend que de `vibe-core`.
   - Je l'ai préféré à du `git` brut à la `commit_files`, parce que la configuration du dépôt n'est pas fiable et que `Git` la neutralise.
   - Le pipeline lui-même ne l'utilise pas. La phrase de `lib.rs` le précise désormais.
   - Si tu préfères un crate sans cette dépendance, l'alternative est un trait `RepoReader` injecté, implémenté par la CLI.
2. **La branche est un argument.** Le plan prévoyait `task_history(store, git, task)`. J'ai retenu :
   - `task_history(..., branch: Option<&str>)` ;
   - `project_history(..., branch_of: &dyn Fn(&Task) -> Option<String>)`.

   La raison : seul l'appelant sait si le workspace a une branche (`in_place` n'en a pas). Pour les tâches d'avant 0.5, il sait aussi la recalculer avec `worktree_location`, qui reste dans la CLI. Il faut aussi passer `&VibeConfig`, pour `base_branch` et `pricing`.
3. **Correspondance des prix.**
   - `AgentStarted.model` contient l'id du modèle chez le fournisseur, sans préfixe, alors que la table est indexée par `"<provider>/<model>"`. `price_of` accepte donc la clé exacte `model` ou toute clé `*/<model>`. Si deux fournisseurs donnent des prix différents, le modèle est considéré comme non tarifé.
   - `cache_read` et `cache_write` sont optionnels. Des tokens de cache sans prix rendent le coût `None`, pour ne rien estimer en silence.
   - `Cost.complete` n'est pas toujours vrai : il vaut `false` quand une session a démarré sans `agent_finished` (crash), et le montant est alors un minorant. Sans aucune session et avec une table non vide, le coût vaut `Some(0.0)` ; avec une table vide, `None`.
   - `agent_finished` n'a pas de `subtask` : l'appariement se fait en FIFO par (run, role). C'est correct dès que des sessions parallèles d'un même rôle partagent le modèle, ce qui est le cas (même phase).
4. **Nouvelle source `Commits` pour les fichiers modifiés,** entre `MergeCommit` et `Trace` : l'union des `files` des événements `committed`, statuts inconnus, `approximate = true`. Elle est plus exacte que la trace (elle inclut les fichiers écrits par `bash`), mais peut lister un fichier annulé plus tard.
5. **Merge en fast-forward.**
   - Constat : la phase de merge essaie d'abord `--ff-only` (`worktree.rs:377`). Or `git show <commit>` ou `<commit>^1..<commit>` ne montreraient que le dernier commit.
   - Solution : je compte les parents (`rev-list --parents -n 1`).
     - Pour un vrai merge : diff contre `^1`.
     - Pour un fast-forward : diff de `<premier commit de la tâche>^1` à `<commit>`, où le premier commit est le premier `committed` du journal ; à défaut, `<commit>^1`.
   - Limite : c'est exact si le premier commit enregistré part de la base, ce qui est le cas pour le checkpoint ou le premier commit de sous-tâche.
   - Le résultat est exposé dans `MergeCommit.fast_forward`.
6. **Erreurs git.** Elles vont dans `TaskHistory.errors: Vec<String>` plutôt que dans une variante `ChangedFilesSource::Error`, pour pouvoir quand même tomber sur la source suivante.
7. **`status` de `RunSummary`** est un `TaskStatus`, et non un `RunStatus`, car c'est ce que porte `run_finished`.
8. **`approvals`** réutilise `state::ApprovalRecord` (décisions uniquement). Une demande en attente est exposée par `pending_approval`.
9. **`run_trace` renvoie `Result<Option<RunTrace>>`,** avec `None` quand la tâche n'a aucun run ou que le run demandé n'a aucun événement. `trace_file_path` et `last_run` sont publics en plus. `run_trace` prend `&dyn PipelineStore` : `FileTaskStore` convient.
10. **Fixtures construites en code.** Les lignes « ancien schéma » sont les vrais `Envelope` sérialisés, auxquels on retire les champs 0.5, type par type (`old_line`). Aucun fichier de fixture statique : si les formes changent, les tests cassent. Pas de `tests/fixtures/`.
11. **Ordre des sources de fichiers modifiés inversé pour les deux premières :** le merge passe avant la branche.
    - Le provider ne supprime pas la branche après le merge (`worktree.rs:377-390`). Après un fast-forward ou un `--no-ff`, `base...branch` est donc vide, alors que la tâche a bien changé des fichiers.
    - `MergeCommit` est donc essayé en premier quand un `merged` existe.
    - Un diff de branche vide passe à la source suivante : c'est le cas d'une branche fusionnée hors de vibe, par une PR puis un `pull`.
    - Le test garde désormais la branche `vibe/ff` après le fast-forward et ajoute une tâche `Outside` dont la branche pointe sur `main`.
12. **`Cost.complete` tient aussi compte des appels hors session.**
    - Les appels de réparation de `run_structured` (`structured.rs:192`) et de résumé de continuation (`continuation.rs:143`) consomment des tokens sans `agent_started` ni `agent_finished`.
    - Pour chaque run dont `run_finished` porte des totaux, si la somme des sessions est inférieure champ par champ aux totaux, `complete = false`. Le montant reste celui des sessions : c'est un minorant, documenté dans `configuration.md`.
    - Ces appels ne sont pas tarifés, puisque leur modèle n'est pas journalisé.

## Vérification

`cargo fmt --all` a reformaté mes fichiers avant les contrôles. Ensuite :

- `cargo fmt --all -- --check` : code de sortie 0, aucune sortie.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features` (via `rtk proxy`, après `touch` des fichiers modifiés pour forcer la revérification) : code de sortie 0, 0 ligne `warning` ou `error`. Dernières lignes :
  ```
      Checking vibe-pipeline v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-pipeline)
      Checking vibe-cli v0.4.0 (/Users/vincentlauriat/DevApps/Devtools/vibe-factory/crates/vibe-cli)
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.08s
  ```
- `cargo test --workspace --all-features` : code de sortie 0, 28 lignes `test result` toutes `ok`, 615 tests passés, 0 échec, aucun panic. Dernière ligne :
  ```
  test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  ```
- Contrôle du test git par mutation : en forçant le repli fast-forward sur `<commit>^1`, `changed_files_come_from_the_most_exact_source_available` échoue. J'ai ensuite restauré le code. Ce contrôle date d'avant les corrections des écarts 11 et 12 ; les tests ont été relancés après.

## Tests ajoutés (16)

- **`events_log`**
  - `reads_appended_chunks_and_waits_for_a_partial_line`
  - `restarts_when_the_log_is_truncated_or_removed`
  - `restarts_when_the_log_is_replaced_by_a_longer_one` (unix)
  - `tagged_envelopes_serialise_flat`
  - `all_events_merge_every_task_by_time` (deux, puis trois tâches ; `since` ; follower : ajouts, nouvelle tâche, tâche supprimée)
- **`trace`**
  - `new_logs_pair_by_id_when_results_come_back_out_of_order`
  - `old_logs_pair_by_order_per_tool`
  - `trace_paths_stay_in_the_trace_store`
  - `run_trace_defaults_to_the_last_run_and_reads_outputs`
- **`vibe-core`**
  - `pricing_by_model_id`
- **`tests/history.rs`**
  - `a_resumed_run_takes_its_totals_from_its_last_run_finished`
  - `old_logs_have_unknown_totals_unless_run_json_describes_the_run`
  - `cost_needs_a_price_for_every_session` : y compris totaux couverts ou dépassés par rapport aux sessions
  - `name_status_output_is_parsed_with_renames`
  - `changed_files_come_from_the_most_exact_source_available` : branche avec `vibebase`, fast-forward sur deux commits avec la branche conservée, branche fusionnée hors de vibe (diff vide, puis `Commits`), merge `--no-ff` avec suppression, trace avec un merge introuvable enregistré en erreur, `Commits` pour une tâche échouée, filtre `all`, tri, base inexistante
  - `history_outside_git_uses_the_log_and_run_json`
