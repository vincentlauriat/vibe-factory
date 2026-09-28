# Étape 2 : compléments de revue (points 9 à 17)

J'ai traité les points 9 à 17 dans l'arbre de travail. Rien n'est commité, et je n'ai rien modifié hors de `crates/` et `docs/`.

## Points traités

9. **Limites du fast-forward documentées** (`docs/src/design/persistence.md`) : ce qui est exact, et les trois cas limites (pointe de branche en merge d'intégration, tâche relancée, aucun commit enregistré contenu). J'y ai ajouté un paragraphe sur le sens d'`approximate` et sur `--end-of-options`.
10. **Commentaire dans `EventReader::read_new`** : un log tronqué puis rempli au-delà de l'offset dans le même fichier n'est pas détecté, le store n'écrivant qu'en ajout.
11. **`--end-of-options` ajouté** avant les révisions :
   - dans `git diff` et `git rev-list` (history.rs) ;
   - dans `Git::is_ancestor` (vibe-workspace ; `merge-base` l'accepte, vérifié avec git 2.55).
   - Test `revisions_that_look_like_options_are_not_read_as_options` : une `vibebase` égale à `--output=<dir>/pwned`. Sans `--end-of-options` dans le diff, il **échoue** : git écrit le fichier et ne signale aucune erreur. Avec, il passe : une erreur est enregistrée et aucun fichier n'est créé.
12. **`read_output(project_root, call)`** : la signature change. Le chemin est canonicalisé et doit commencer par `<root>/.vibe/tool-output` canonicalisé ; sinon c'est une erreur « resolves outside ». Test ajouté dans `run_trace_defaults_to_the_last_run_and_reads_outputs` (unix) : un symlink vers un fichier hors du store est refusé.
13. **Prix validés dans `VibeConfig::from_toml`** : chaque prix doit être fini et positif ou nul, sinon erreur de config qui nomme `pricing."<clé>".<champ>`.
   - L'ordre de correspondance (clé exacte, puis `*/<model>` avec le fournisseur avant le premier `/`) et le cas d'un id contenant `/` sont documentés dans `price_of` et dans `configuration.md`.
   - Tests : `prices_must_be_finite_and_non_negative`, plus un cas `openrouter/meta/llama` dans `pricing_by_model_id`.
14. **Appariement et index lus une seule fois.**
   - Nouveau `trace::pair_all_calls(events, root)` : un seul passage pour tous les runs, avec des clés d'appariement incluant le run. `Call` gagne un champ `run: RunId`.
   - L'historique l'appelle une fois par tâche, et seulement si la trace est la source retenue.
   - `project_history` charge `entries()` une fois et passe `number` à `build_history`, qui ne relit plus l'index et ne peut plus échouer.
15. **Échec de `for-each-ref`** : il est enregistré dans `Repo.errors`, puis recopié dans les `errors` de chaque tâche. Je n'ai pas de test : je n'ai pas trouvé comment faire échouer `for-each-ref` de façon fiable, car un ref cassé n'est qu'un avertissement.
16. **Ergonomie.**
   - Ajouts : `AllEventsFollower::from_end(&store)` et `EventReader::at_end(path)`, qui gardent une ligne finale incomplète pour la compléter plus tard. Test : `a_follower_from_the_end_skips_what_is_already_logged` (ligne incomplète, ajouts, nouvelle tâche).
   - Types de store : je n'ai pas harmonisé.
     - `run_trace` garde `&dyn PipelineStore`, parce qu'il n'utilise que `load_events`.
     - L'historique garde `&FileTaskStore`, parce qu'il lui faut `entries` et `is_running`.
     - Un `&FileTaskStore` se passe tel quel aux deux, donc l'appelant n'a rien à convertir.
17. **Confinement vérifié** : `grep vibe_workspace crates/vibe-pipeline/src` ne trouve que `history.rs`.

## Vérification

- `cargo fmt --all -- --check` : sortie 0.
- `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features`, après `touch` des sources modifiées : sortie 0, aucun avertissement. Dernière ligne : `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 2.96s`.
- `cargo test --workspace --all-features` : sortie 0, 28 suites, **626 passés**, 0 échec, aucun panic. C'était 623 avant ce lot : +3 tests (`prices_must_be_finite_and_non_negative`, `a_follower_from_the_end_skips_what_is_already_logged`, `revisions_that_look_like_options_are_not_read_as_options`), les autres cas étant ajoutés à des tests existants.

## À noter

`HEAD` est désormais `fb422a5`, qui n'est pas de moi. `git status` montre aussi `.github/workflows/ci.yml` et `TODOS.md` modifiés : je n'y ai pas touché.
