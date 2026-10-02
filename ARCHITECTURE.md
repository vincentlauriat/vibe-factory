# Vibe Factory — Architecture (miroir français)

> Source de vérité : [ARCHITECTURE_EN.md](ARCHITECTURE_EN.md). Le livre de conception complet
> est dans [`docs/`](docs/src/design/overview.md) (mdBook) ; ce fichier est le résumé exécutif.

## Objectif

Un framework Rust ouvert et extensible pour le développement logiciel autonome multi-agents :
une description de tâche traverse *assess → spec → plan → build → qa ⇄ fix → merge*, exécutée
par des agents IA coopérants dans un espace de travail git isolé.

## Crates en couches

```text
vibe-cli → { vibe-pipeline → { vibe-agents, vibe-workspace }, vibe-providers, vibe-tools, vibe-plugins } → vibe-core
apps/macos, editors/vscode, UI web → API HTTP de `vibe serve` (vibe-cli)
```

Chaque crate ne dépend que de `vibe-core`, sauf `vibe-pipeline` qui dépend aussi de
`vibe-agents`, et de `vibe-workspace` pour les requêtes git en lecture seule de sa couche de lecture.
Les implémentations concrètes ne se rencontrent que dans `vibe-cli`, via le `Registry`.

| Crate | Responsabilité |
|-------|----------------|
| `vibe-core` | Types du domaine (Task, Spec, Plan, Subtask, QaReport, Message), traits (`ModelProvider`, `Tool`, `WorkspaceProvider`, `MemoryStore`, `TaskStore`, `Hook`, `Plugin`), `AgentSpec`, `Registry`, `EventBus`, `VibeConfig`, `PromptTemplate`. Aucune E/S. |
| `vibe-providers` | API Messages d'Anthropic, chat completions compatibles OpenAI (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama), provider mock, retry/backoff, classification des erreurs HTTP, registre de providers. |
| `vibe-tools` | Outils intégrés (`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`), analyseur de commandes shell et politique de sécurité, troncature des sorties. |
| `vibe-workspace` | Provider de worktrees git (ouverture/réouverture, changements, merge, suppression), provider en place, résolution de conflits assistée par IA optionnelle, aides au commit. |
| `vibe-plugins` | Vibe Plugin Protocol (JSON-RPC 2.0 sur stdio, au format MCP), client de processus plugin, manifestes et découverte, hôte de plugins, aide `PluginServer` pour écrire des plugins en Rust. |
| `vibe-agents` | Boucle agentique `AgentRunner` (outils non mutants en parallèle, hooks, budget de contexte, annulation), extraction et réparation des sorties structurées, continuation après épuisement du contexte, prompts et specs d'agents intégrés, surcharges d'agents en TOML, trace des sorties complètes des outils (`ToolTrace`). |
| `vibe-pipeline` | `TaskStore` sur fichiers sous `.vibe/tasks/NNN-slug/`, profils de complexité, orchestration des phases, exécution parallèle des sous-tâches en respectant `depends_on`, boucle QA/fix avec escalade, approbations, budgets, état d'exécution et reprise, journal d'événements, `RunManager` (le point d'entrée de toutes les interfaces), couche de lecture : `events_log` (lecture incrémentale et à l'échelle du projet des événements, `EventCursor`), `history` (ce qu'une tâche a livré, à partir des événements et de git), `trace` (appels d'outils d'une exécution avec leurs sorties). |
| `vibe-cli` | Binaire `vibe` : `init`, `task`, `run`, `approve`, `reject`, `cancel`, `pr`, `memory`, `events`, `history`, `trace`, `status`, `config`, `agents`, `plugins`, `doctor` ; rendu des événements en direct ; `vibe tui` (tableau, activité, historique, trace) ; `vibe serve` (API HTTP, un flux global d'événements alimenté par un lecteur unique partagé, UI web intégrée). |

## Règles de conception clés

1. **Le cœur = données + traits.** Tout ce qui est concret implémente un trait du cœur et est collecté dans un `Registry` ; le pipeline ne parle qu'au registre.
2. **Les agents sont déclaratifs** (`AgentSpec`), exécutés par un unique runtime ; l'utilisateur surcharge ou ajoute des agents dans `.vibe/agents/*.toml`.
3. **Les artefacts sur disque forment la machine à états.** Chaque phase lit les artefacts précédents et écrit les siens ; les exécutions sont inspectables et reprenables.
4. **Verdicts structurés.** Complexité, spec, plan et QA sont du JSON désérialisé en structs typées (avec réparation + une relance) ; le markdown est rendu à partir d'elles, jamais analysé.
5. **Isolation par défaut** via worktrees git ; `in_place` pour les expérimentations ; autres isolations via des plugins `WorkspaceProvider`.
6. **Défense en profondeur pour le shell** : segments analysés, programmes bloqués, validateurs par commande, allowlist optionnelle, confinement des chemins, timeouts.
7. **Plugins dans n'importe quel langage** via le protocole stdio ; les serveurs MCP fonctionnent comme plugins d'outils.
8. **Un seul point d'entrée, les événements comme contrat** (ADR-007, ADR-008) : chaque interface passe par `RunManager` et reconstruit ses vues à partir des événements persistés et de git via la couche de lecture ; les sorties complètes des outils sont dans un stockage de traces référencé par les événements.

## Interfaces

| Interface | Où | Accède au moteur via |
|-----------|----|----------------------|
| Ligne de commande | `vibe` (`vibe-cli`) | `RunManager` et la couche de lecture, dans le processus |
| UI terminal | `vibe tui` (`vibe-cli`, module `tui`) | les mêmes, dans le processus |
| UI web et API HTTP | `vibe serve` (`vibe-cli`, module `server`) | les mêmes ; les clients passent par HTTP et les server-sent events |
| App macOS | `apps/macos/VibeFactory` (SwiftUI ; DMG signé, mises à jour Sparkle) | l'API HTTP d'un `vibe serve` qu'elle démarre ou auquel elle se connecte |
| Extension VS Code | `editors/vscode` (TypeScript) | l'API HTTP de `vibe serve` |

## Persistance (`.vibe/`)

```text
.vibe/
  config.toml                 configuration du projet
  agents/*.toml               surcharges d'agents
  plugins/<name>/vibe-plugin.toml
  memory.jsonl                mémoire du projet partagée par les tâches
  server.token                jeton d'un `vibe serve` en cours
  worktrees/<slug>-<id>/      espaces de travail isolés (chacun avec .vibe/tool-output/<uuid>.txt
                              pour les sorties trop longues pour le modèle)
  tasks/index.json            id de tâche → répertoire et numéro
  tasks/NNN-slug/
    task.json  spec.md  spec.json  plan.json  qa_report_<n>.json  qa_report_<n>.md
    progress.md  memory/{gotchas.md,patterns.md}  events.jsonl  run.json  run.lock
  tool-output/<task dir>/<run>/<call>.txt   stockage des traces : sortie complète de chaque appel d'outil
```

## Portes de qualité

`cargo fmt --check`, `cargo clippy --workspace --all-targets` avec `-D warnings`,
`cargo test --workspace`, `cargo doc` avec `-D warnings`, `mdbook build docs`, sur Linux,
macOS et Windows (GitHub Actions). `unsafe_code` interdit. Chaque élément public documenté.
