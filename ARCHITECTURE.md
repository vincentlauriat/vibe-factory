# Vibe Factory — Architecture (miroir français)

> Source de vérité : [ARCHITECTURE_EN.md](ARCHITECTURE_EN.md). Le livre de conception complet
> est dans [`docs/`](docs/src/design/overview.md) (mdBook) ; ce fichier est le résumé exécutif.

## Objectif

Un framework Rust ouvert et extensible pour le développement logiciel autonome multi-agents :
une description de tâche traverse *assess → spec → plan → build → qa ⇄ fix → merge*, exécutée
par des agents IA coopérants dans un espace de travail git isolé.

## Crates en couches

```text
vibe-cli → { vibe-pipeline → vibe-agents, vibe-providers, vibe-tools, vibe-workspace, vibe-plugins } → vibe-core
```

Chaque crate ne dépend que de `vibe-core`, sauf `vibe-pipeline` qui dépend aussi de
`vibe-agents`. Les implémentations concrètes ne se rencontrent que dans `vibe-cli`, via le `Registry`.

| Crate | Responsabilité |
|-------|----------------|
| `vibe-core` | Types du domaine (Task, Spec, Plan, Subtask, QaReport, Message), traits (`ModelProvider`, `Tool`, `WorkspaceProvider`, `MemoryStore`, `TaskStore`, `Hook`, `Plugin`), `AgentSpec`, `Registry`, `EventBus`, `VibeConfig`, `PromptTemplate`. Aucune E/S. |
| `vibe-providers` | API Messages d'Anthropic, chat completions compatibles OpenAI (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama), provider mock, retry/backoff, classification des erreurs HTTP, registre de providers. |
| `vibe-tools` | Outils intégrés (`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`), analyseur de commandes shell et politique de sécurité, troncature des sorties. |
| `vibe-workspace` | Provider de worktrees git (ouverture/réouverture, changements, merge, suppression), provider en place, résolution de conflits assistée par IA optionnelle, aides au commit. |
| `vibe-plugins` | Vibe Plugin Protocol (JSON-RPC 2.0 sur stdio, au format MCP), client de processus plugin, manifestes et découverte, hôte de plugins, aide `PluginServer` pour écrire des plugins en Rust. |
| `vibe-agents` | Boucle agentique `AgentRunner` (outils non mutants en parallèle, hooks, budget de contexte, annulation), extraction et réparation des sorties structurées, continuation après épuisement du contexte, prompts et specs d'agents intégrés, surcharges d'agents en TOML. |
| `vibe-pipeline` | `TaskStore` sur fichiers sous `.vibe/tasks/NNN-slug/`, profils de complexité, orchestration des phases, exécution parallèle des sous-tâches en respectant `depends_on`, boucle QA/fix avec escalade, état d'exécution et reprise, journal d'événements. |
| `vibe-cli` | Binaire `vibe` : `init`, `task add|list|show|discard`, `run`, `status`, `config`, `agents`, `plugins`, `doctor` ; rendu des événements en direct. |

## Règles de conception clés

1. **Le cœur = données + traits.** Tout ce qui est concret implémente un trait du cœur et est collecté dans un `Registry` ; le pipeline ne parle qu'au registre.
2. **Les agents sont déclaratifs** (`AgentSpec`), exécutés par un unique runtime ; l'utilisateur surcharge ou ajoute des agents dans `.vibe/agents/*.toml`.
3. **Les artefacts sur disque forment la machine à états.** Chaque phase lit les artefacts précédents et écrit les siens ; les exécutions sont inspectables et reprenables.
4. **Verdicts structurés.** Complexité, spec, plan et QA sont du JSON désérialisé en structs typées (avec réparation + une relance) ; le markdown est rendu à partir d'elles, jamais analysé.
5. **Isolation par défaut** via worktrees git ; `in_place` pour les expérimentations ; autres isolations via des plugins `WorkspaceProvider`.
6. **Défense en profondeur pour le shell** : segments analysés, programmes bloqués, validateurs par commande, allowlist optionnelle, confinement des chemins, timeouts.
7. **Plugins dans n'importe quel langage** via le protocole stdio ; les serveurs MCP fonctionnent comme plugins d'outils.

## Persistance (`.vibe/`)

```text
.vibe/
  config.toml                 configuration du projet
  agents/*.toml               surcharges d'agents
  plugins/<name>/vibe-plugin.toml
  worktrees/<slug>-<id>/      espaces de travail isolés
  tasks/NNN-slug/
    task.json  spec.md  spec.json  plan.json  qa_report_<n>.json  qa_report_<n>.md
    progress.md  memory/{gotchas.md,patterns.md}  events.jsonl  run.json
  tool-output/<uuid>.txt      texte complet des sorties d'outils tronquées
```

## Portes de qualité

`cargo fmt --check`, `cargo clippy --workspace --all-targets` avec `-D warnings`,
`cargo test --workspace`, `cargo doc` avec `-D warnings`, `mdbook build docs`, sur Linux,
macOS et Windows (GitHub Actions). `unsafe_code` interdit. Chaque élément public documenté.
