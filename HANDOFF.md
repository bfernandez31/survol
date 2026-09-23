# HANDOFF — survol

> **survol** — *a bird's-eye view of large pull requests.* Survoler une grosse MR/PR pour en comprendre l'architecture, puis descendre là où c'est nécessaire.
>
> Commande : `survol`. Si le nom est pris sur crates.io, publier le paquet sous `survol-review` en gardant le binaire `survol`.
>
> Ce document sert de point de départ pour construire l'outil, typiquement avec Claude Code dans le dépôt du projet. Il résume le besoin, les décisions déjà prises, l'architecture cible, la roadmap et les questions encore ouvertes.

---

## 1. Contexte

- On produit de plus en plus de code **généré par IA** : grosses features, réécritures complètes sur une nouvelle stack, nouveaux projets.
- Résultat : des **merge requests GitLab de plusieurs centaines de fichiers** (700 et plus), impossibles à lire intégralement.
- Le relecteur **maîtrise mal le fonctionnel** du code relu, mais doit **garder la maîtrise technique** et prendre une décision.
- L'interface web de GitLab montre ce qui a changé, mais pas **comment l'ensemble s'imbrique** : qui appelle quoi, comment c'est branché, ce qu'un changement touche dans le code non modifié.
- L'utilisateur cible travaille exclusivement dans le terminal : **Neovim** pour éditer, **LazyGit** pour commit, push et pull.

## 2. Objectif

**Permettre à un humain de juger rapidement, au niveau architecture, si une grosse MR est cohérente et bien branchée, sans tout lire.**

Lire le code modifié isolément donne souvent l'impression que tout est correct. Pour vraiment valider, il faut avoir en tête les imbrications et une vue d'ensemble. C'est ce que l'outil doit fournir.

### Objectifs

1. Accélérer la review des très grosses MR.
2. Apporter de la **compréhension** du code : ce qu'il fait, comment il est organisé, comment il est branché.
3. Aider le relecteur à se forger un **avis architectural** et à le formuler (commentaires GitLab).

### Non-objectifs (important)

- **Trouver des bugs.** Des agents le font déjà (CodeRabbit, PR-Agent, GitLab Duo dans la CI). `survol` ne remonte pas de liste de bugs ou d'alertes à la manière d'un linter.
- Remplacer l'IDE ou Neovim.
- Couvrir GitHub dès le départ. **GitLab d'abord**, en gardant une abstraction « forge » pour plus tard.
- Des cas uniquement « migration mécanique » (montée de version de framework). Le cas principal, ce sont les grosses features et les réécritures.

## 3. Décisions déjà prises

| Sujet | Décision | Raison |
|---|---|---|
| Forme | **Outil TUI autonome**, façon LazyGit | Interface riche (3 vues, graphe) plus simple avec un framework TUI qu'en buffers Lua. Utilisable hors de Neovim. |
| Intégration Neovim | Ouvert depuis Neovim en **fenêtre flottante**. Les sauts vers le code s'ouvrent dans le Neovim parent via `nvim --server $NVIM --remote` | Même modèle que lazygit.nvim. On récupère le LSP, les keymaps et la config de l'utilisateur au moment du saut. |
| Langage | **Rust** | Tree-sitter natif, ratatui mature. Proche de tuicr, qui peut servir de référence. |
| Découpage | **Moteur (CLI → JSON)** + **TUI** + **plugin Neovim minimal** | Moteur testable et réutilisable. TUI découplée de l'analyse. |
| Source de vérité des liens | **Analyse statique** (tree-sitter, puis LSP) | Le LLM n'invente jamais de lien entre du code. |
| Rôle du LLM | **Couche légère** : nommer et expliquer les groupes, répondre aux questions sur un nœud | Il travaille sur des données déjà calculées. |
| Checkout | Local, dans un **git worktree** dédié | Accès au vrai code et aux outils locaux, sans toucher à la branche de travail. |
| Framework TUI | **ratatui** + crossterm | Confirmé. |
| Langages prioritaires | **Java / Kotlin (Spring)** puis **TypeScript / JavaScript (Angular)** | Langages des projets relus. Grammaires tree-sitter : `tree-sitter-java`, `tree-sitter-kotlin`, `tree-sitter-typescript` (TS + TSX), `tree-sitter-javascript`. |
| Fournisseur LLM | **CLI Claude Code uniquement** (`claude -p`) en v1 | Seul fournisseur autorisé. L'authentification est déléguée au CLI, `survol` ne stocke aucune clé. L'abstraction fournisseur reste en place pour plus tard. |
| GitLab | **Instance auto-hébergée** | `GITLAB_HOST` obligatoire dans la config, certificats d'entreprise (CA personnalisée) à supporter, pas d'URL gitlab.com codée en dur. |
| Nom | **survol** | Nom français assumé, à la manière de Vue : court, sans accent, sans sens gênant en anglais, et porteur de l'idée (vue d'ensemble puis descente dans le détail). Accroche anglaise : *a bird's-eye view of large pull requests*. Homonymes hors domaine (appli de notes IA, appli de vol). Écartés : `revu`, `vigie` (même créneau), `trame`, `sextant`, `gestalt`, `grasp` (pris ou peu parlants), `diffmap`, `difflens`, `diffsense`, `diffscope`, `changemap` (pris dans la review / l'analyse d'impact, ou peu appréciés). |
| Accès git | **Binaire `git`** (pas `git2` / `gix`) | Comportement identique à la ligne de commande : identifiants, SSH, `refs/merge-requests/*`, `diff -M`. Pas de libgit2 à compiler. |
| Mode local | `survol base..head` en plus des MR | Diff depuis la merge-base comme GitLab. Permet de tester et d'utiliser l'outil sans GitLab. |
| État « relu » | Clé = `content_hash` du hunk (fichier + lignes, sans numéros), stocké dans `.git/survol/reviews/<mr-iid>/state.json` | Un nouveau commit ne remet « à relire » que les hunks réellement modifiés, sans calcul de correspondance. |
| Ordre des fichiers | En arbre : fichiers d'un répertoire, puis ses sous-répertoires | Le tri git par chemin complet éclate les répertoires dans la sidebar. |
| Coloration (étape 1) | `syntect` + `two-face` (TS, Kotlin…), thème `ansi` | Suit la palette du terminal (clair ou sombre). tree-sitter prendra le relais avec l'index (étape 3). |
| TLS GitLab | `reqwest` + rustls avec le vérificateur de la plateforme, `ca_cert` ajouté au magasin système | La CA d'entreprise installée dans le trousseau fonctionne sans configuration. |
| Jeton GitLab | `GITLAB_TOKEN`, sinon `glab config get token --host <host>` | Réutilise glab sans parser son fichier (ni le trousseau). |
| Worktree | Créé en arrière-plan, la vue Diff est utilisable tout de suite | Le diff ne dépend pas du checkout. |
| Vue Diff | Réécrite en s'inspirant de tuicr, pas de fork | Voir §9. |
| Appel du CLI Claude | `claude -p --output-format json --tools "" --strict-mcp-config --disable-slash-commands --no-session-persistence --setting-sources "" --system-prompt …`, prompt sur stdin | Complétion pure en un tour : aucun outil, MCP, skill ni réglage utilisateur. Le LLM ne répond qu'à partir du prompt. |
| Compte Claude | `[llm] config_dir` → `CLAUDE_CONFIG_DIR` du processus enfant (`~` développé), affiché par `doctor` avec le compte connecté | Utiliser le compte pro pour le code pro sans toucher au compte par défaut. |
| Groupe mécanique | Hunks des fichiers `is_generated`, hunks « blancs seulement » (lignes retirées = ajoutées une fois tous les blancs supprimés), et tous les fichiers sans hunk (renommages/copies purs, binaires, modes) via `Group::file_ids` | Déterministe, sans LLM, placé en dernier. |
| Identifiants envoyés au LLM | Index numériques des hunks (`[12]`), une ligne compressée par hunk : section `@@`, +/-, 3 premières lignes modifiées tronquées à 80 caractères, précédées d'une ligne `file` (chemin, statut, langage) | Compact (~150 caractères par hunk) ; le LLM ne répond qu'avec des ids. |
| Budget de prompt | `[llm] max_prompt_chars` (150 000 par défaut). Au-delà : découpage par répertoire (sans couper un répertoire qui tient dans un morceau), morceaux traités en parallèle (4), puis prompt de fusion qui ne renvoie que les groupes à fusionner | Fusion simple et vérifiable en code. |
| Échec du LLM | Réponse invalide → une nouvelle tentative avec la réponse rejetée et l'erreur (le modèle corrige au lieu de tout refaire). Puis on garde les groupes valides et on regroupe le reste par répertoire (`partial`) ; sinon tout par répertoire (`fallback`). Erreur d'appel (CLI absent, non connecté) → repli direct, sans nouvelle tentative. Chemin suivi et erreurs dans `Grouping.source` / `warnings` | Toujours 100 % des hunks attribués ; l'outil reste utilisable sans LLM. |
| Ordre des groupes (provisoire) | Rang de la couche dominante : modèle/persistance/config/build → services/autre → api/ui/points d'entrée → tests/docs ; groupe mécanique en dernier. Tri stable (l'ordre proposé par le LLM départage). Isolé dans `group::order_groups` | Remplacé par le tri topologique à l'étape 3. |
| Cache des groupes | `.git/survol/cache/<head_sha>/groups.json`, clé = hash des `content_hash` + classement mécanique + `PROMPT_VERSION` + modèle + budget + langue + consignes projet. Les replis complets par répertoire ne sont pas mis en cache | Réouverture sans appel LLM ; un nouvel essai LLM au prochain lancement si le LLM était indisponible. |
| Effort de raisonnement | `[llm] group_effort = "low"` par défaut | Mesuré sur whisper.cpp (683 hunks, sonnet) : effort par défaut ≈ 90 % de tokens de réflexion, ~5 min et ~0,65 $ par appel ; `low` : ~25 s, ~0,35 $, groupes un peu plus gros. `medium` possible pour des groupes plus fins. |
| Debug LLM | `SURVOL_LLM_LOG=<dir>` garde chaque prompt et réponse brute | Diagnostic du coût, de la latence et des prompts. |
| Validation d'un groupe | Pas d'état dédié : `Group::set_reviewed` marque ses hunks (et fichiers sans hunk) dans le `ReviewState` commun | Les trois vues partagent le même état « relu ». |
| Navigation entre vues | `Tab` / `Shift-Tab` et `1` / `2` changent de vue ; `Ctrl-h` / `Ctrl-l` changent de panneau (liste ↔ contenu) | `Tab` est réservé aux vues (3 à terme) ; `Ctrl-h/l` comme les fenêtres Neovim. |
| Regroupement dans la TUI | Lancé en arrière-plan à l'ouverture, cache d'abord ; `R` régénère sans cache après confirmation y/n | La vue Diff reste utilisable pendant l'appel LLM ; pas d'appel payant par erreur. |
| Mode sans LLM | `--no-llm` ou `[llm] enabled = false` : groupe mécanique + regroupement par répertoire, sans cache | Confidentialité, hors ligne, tests. |
| `space` dans la vue Stack | Valide tout le nœud (groupe, couche, hunk). Groupe terminé → replié, passage au groupe suivant non relu ; sinon nœud suivant non relu du même groupe | Valider un groupe d'un coup, sans quitter un groupe à moitié relu. |
| Langue des résumés | `[llm] language` (anglais par défaut), surchargé par `--lang` (`survol`, `survol-cli group`). Codes courants (`fr`, `en`, `de`, `es`, `it`, `pt`, `nl`) et noms natifs traduits en nom anglais pour le prompt, le reste passé tel quel. Prompts en anglais qui demandent titres et résumés dans la langue ; ids, noms de couche et clés JSON inchangés. Langue incluse dans la clé de cache ; textes sans LLM (repli par répertoire, groupe mécanique) traduits en français | Le relecteur peut lire en français sans casser le parsing ni l'ordre des couches ; changer de langue régénère au lieu de servir le cache dans l'autre langue. |
| Grammaires tree-sitter | `tree-sitter` 0.27, `tree-sitter-java` 0.23, `tree-sitter-kotlin-ng` 1.1 (grammaire maintenue par tree-sitter-grammars), `tree-sitter-typescript` 0.23 (TS + TSX), `tree-sitter-javascript` 0.25 (JS + JSX) | Versions compatibles entre elles (ABI 14/15). `tree-sitter-kotlin` d'origine n'est plus maintenue. |
| Requêtes d'index | Une requête par langage dans `crates/survol-core/queries/*.scm`, style tags.scm : `@definition.*`, `@reference.*`, `@binding.*`, `@import`, `@package`. Annotations / décorateurs (avec arguments), conteneurs, arité et imports lus ensuite dans l'arbre (`index/extract.rs`) | Requêtes versionnées et lisibles ; ce que les requêtes expriment mal reste en code. |
| Source des fichiers indexés | `git ls-tree` du head + `git cat-file --batch`, sans checkout ; version base des fichiers modifiés / supprimés / renommés ; exclus : globs mécaniques + `node_modules`, `dist`, `build`, `target`, `*.d.ts`…, fichiers > 512 Ko | Indexable avant que le worktree existe ; mêmes octets que le commit relu. |
| Cache de l'index | Par blob : `.git/survol/cache/index/v<N>-<hash des requêtes>/<ab>/<blob>.<lang>.json`, lecture et parsing en parallèle (rayon). Graphe complet : `.git/survol/cache/<head_sha>/graph.json` (clé : versions, base, head, règles, globs) | Nouvelle version de MR : seuls les blobs changés sont reparsés ; modifier une requête invalide le cache sans bump manuel. Mesuré sur spring-framework (9 611 fichiers) : 4,9 s à froid, 1,9 s index en cache, 0,3 s graphe en cache. |
| Identifiants de symboles | `chemin#Classe.Interne.methode/arité` (`~k` pour des surcharges de même arité), chemin seul pour un fichier ; un hunk pointe vers le symbole le plus interne de ses lignes ajoutées (head) ou retirées (base : même id s'il existe encore, sinon symbole `removed`) | Stables d'une version à l'autre ; méthodes supprimées ou renommées connues. |
| Politique de confiance | 0,9–1 : cible déterminée (même fichier, import explicite, receveur typé par champ / paramètre / local) ; ~0,85 : même package ; 0,5 : nom + type visible depuis le fichier ; 0,3 : devinette globale ; divisée par le nombre de candidats, plus de 5 candidats → aucune arête ; import d'un paquet externe ou receveur de type externe → aucune arête ; noms trop communs (`get`, `map`, `save`…) sur un receveur inconnu → aucune arête ; le code principal n'appelle jamais le code de test. L'arité filtre les surcharges | Jamais d'arête inventée ; l'ambiguïté se voit dans la confiance. |
| Règles de framework | Trait `graph::FrameworkRule` (`name`, `apply(&Index, &mut GraphBuilder)`), liste dans `graph::rules::default_rules()` : `test-naming`, `spring`, `angular`, `http-link` (dans cet ordre), exécutées après la résolution générique. Elles ajoutent des arêtes, des `Role` et des tags ; un changement de règle incrémente `GRAPH_VERSION` | Spring / Angular ajoutés sans toucher à la résolution. |
| Ordre des groupes (graphe) | `group::order_groups_with_graph` / `Grouping::order_with_graph` : tri topologique des groupes (arêtes de dépendance entre leurs symboles modifiés, confiance ≥ 0,3), cycles condensés (Tarjan), rang de couche puis ordre d'origine pour départager ; groupe mécanique en dernier. `order_groups` reste le repli sans graphe | Les fondations d'abord, d'après le code réel. |
| Conventions des règles | Rôles : `entry_point`, `component`, `repository`, `config`, `entity`, `view`, `external` (nouveau : `@FeignClient`, classes portant un client HTTP). Arêtes : `injects` (consommateur → implémentation ou méthode `@Bean`), `http_calls` (appel HTTP → endpoint), `configures` (fichier de config / spec OpenAPI / `environment` → consommateur, ligne = ligne de la clé), `routes` (table de routes ou template → composant), `publishes` (nouveau : `publishEvent` → listener), `uses` (module Angular → déclarations). Tags, valeurs multiples jointes par `, ` : `entry` (`http`, `kafka`, `rabbit`, `jms`, `sqs`, `scheduled`, `event`, `runner`, `main`, `route`), `http.route` (`GET /api/owners/{id}`), `http.context_path`, `http.spec`, `http.calls` (`GET /petclinic/api/owners/*`), `http.client`, `http.base_url`, `external` (`feign`, `rest_template`, `web_client`, `rest_client`), `kafka.topics`, `rabbit.queues`, `jms.destination`, `schedule`, `event.type`, `event.publishes`, `spring.stereotype`, `spring.bean`, `bean.type`, `spring.profile`, `config.keys`, `config.prefix`, `persistence` / `persistence.entity` / `persistence.table`, `angular.selector`, `angular.template`, `angular.route`, `angular.provided_in`, `angular.pipe`, `angular.environment` | Un vocabulaire stable pour la vue Graphe et le LLM, sans nouveau champ dans le modèle. |
| Index pour les règles | `INDEX_VERSION` 2. En plus : `Def.type_name` (type d'un champ, retour d'une méthode), `Def.modifiers` (`final`, `abstract`, `static`, `readonly`…), `Binding.annotations` (paramètres : `@Qualifier`, `@Value`, `@Inject`), `Ref.arg` (1er argument d'un appel s'il est littéral, nom, concaténation, template, `new`), `FileIndex.values` (initialiseurs « chaîne » des constantes, champs et locales), `FileIndex.routes` (objets littéraux en forme de route), constantes de haut niveau TS/JS comme définitions. Fichiers ressources sans tree-sitter (`Lang::Html` / `Yaml` / `Properties`) : clés à plat des `application*` / `bootstrap*` et des specs OpenAPI (`*api*.yml`), balises personnalisées des `.html` | Les règles lisent tout dans l'index, sans relire les sources. Mesuré sur spring-framework : index +10 % en taille, parsing ~+8 %, règles +20 à 80 ms. |
| Liaison front ↔ back | Dans un même index : le cas cible est le monorepo. Deux dépôts séparés : `Index::merge(autre, "préfixe")` construit un index commun (exemple `graph_facts --with=<dépôt>`). URL reconstruite (littéraux, templates, concaténations, champs, constantes, `environment.*`), hôte retiré, inconnu → joker ; alignement des segments par la fin ; `{id}` ↔ joker ; préfixe différent (`/api`, context-path) toléré avec une confiance plus basse (0,7 / 0,6), base inconnue 0,85 ; meilleur score gagnant, ex æquo partagés, plafond 0,9 | Pas de lien sans segment littéral commun ; la confiance dit l'incertitude. |
| Injection Spring | Points d'injection : constructeur (unique ou `@Autowired`), constructeur primaire Kotlin, record, Lombok `@RequiredArgsConstructor` (champs `final`) / `@AllArgsConstructor`, champs et setters `@Autowired` / `@Inject` / `@Resource`, paramètres de méthodes `@Bean`. Cibles : implémentations (sous-types transitifs) déclarées beans, producteurs `@Bean`, dépôts Spring Data ; `@Qualifier` / `@Named`, puis `@Primary`, puis nom du paramètre départagent ; sinon confiance 0,9 divisée par le nombre de candidats | Même politique de confiance que la résolution générique. |
| Questions au LLM (contexte) | Sujet : symbole (id stable), groupe (id) ou hunk (`content_hash`). Contexte construit en code (`ask`) : code du nœud lu dans les objets git, lignes numérotées ; hunks avec numéros ancien / nouveau ; appelants (extrait autour de l'appel), appelés, tests, autres arêtes, chacun avec `fichier:ligne`, confiance et « modifié / intact » ; résumé du groupe ; consignes projet ; langue. Budget 80 000 caractères. Prompt `prompts/ask.md` (`ASK_PROMPT_VERSION`), modèle `[llm] ask_model` | Le LLM ne répond qu'à partir de données déjà calculées et vérifiables. |
| Réponses et liens | Texte brut (pas de JSON) citant le code en `[chemin:ligne]` (`-fin`, ` (old)` pour une ligne retirée). Références extraites en code : valides seulement si la ligne figurait dans le contexte (chemin complet ou suffixe unique), sinon barrées et non navigables | Une prose en JSON n'apporte rien ; le contrôle porte sur les liens, seul endroit où le LLM pourrait inventer. |
| Cache et historique des questions | Réponse en cache dans `.git/survol/cache/<head>/ask/<clé>.json`, clé = hash de (version, modèle, prompt complet) ; historique de la review dans `reviews/<clé>/questions.json` | Même question sur le même code : aucun nouvel appel ; changer le contexte, la langue ou les consignes redemande. |
| Questions dans la TUI | `a` sur un nœud (Graphe : symbole ; Stack : groupe, ou hunk sous le curseur ; Diff : hunk) → saisie avec suggestions (↑/↓ ou chiffre) ; appel en arrière-plan avec spinner (Esc pour continuer à relire) ; réponse en fenêtre : `Tab`/`n` lien suivant, `Entrée` vue Graphe du symbole (sinon la ligne dans le Diff), `d` la ligne dans le Diff, `e` éditeur ; `A` dernière réponse puis historique. `--no-llm` : touche désactivée avec un message | Comprendre un nœud sans quitter la review, et vérifier la réponse d'un saut. |
| Brouillons de commentaires | Locaux, dans `.git/survol/reviews/<clé>/comments.json` (`comments`), ancrés par fichier, côté (`old` pour une ligne retirée), `content_hash` du hunk, index de la ligne dans le hunk et texte ; ligne, plage (dans un seul hunk), fichier, réponse à une discussion, plus un commentaire global. Après de nouveaux commits : même hunk → même ligne ; sinon la ligne de même texte la plus proche dans le fichier (« moved ») ; sinon « stale », affichée et jamais publiée | Les brouillons survivent aux nouvelles versions sans correspondance fragile par numéro de ligne. |
| Position GitLab | Calculée depuis le diff courant à la publication : `position_type: text`, `base_sha` / `start_sha` / `head_sha` de la MR (plage locale : merge-base / merge-base / head), `old_path` / `new_path` toujours tous deux (renommage : différents), `new_line` pour une ligne ajoutée, `old_line` pour une ligne retirée, les deux pour une ligne inchangée. Plage : `line_range.start/end` avec `line_code` = `sha1(chemin)_<compteur ancien>_<compteur nouveau>` (compteurs du parseur GitLab : une ligne ajoutée garde le compteur ancien), `type` `new` pour une ligne ajoutée sinon `old`, et la position principale sur la dernière ligne. Fichier entier : `position_type: file` (GitLab ≥ 16.4), sinon commentaire général préfixé du chemin. SHA-1 implémenté localement (seul usage : `line_code`) | Point le plus fragile (§8) : JSON exact vérifié en test pour ligne ajoutée / retirée / inchangée, renommage, fichier supprimé ou ajouté, plages, fichier. |
| Publication | Brouillons → `POST .../draft_notes` puis un `POST .../draft_notes/bulk_publish` (GitLab ≥ 15.10, lu par `GET /version`) ; instance plus ancienne → `POST .../discussions` (ou `.../discussions/<id>/notes` pour une réponse) un par un, avec un avertissement explicite dans la confirmation. L'état est sauvegardé après chaque requête (`remote_id` des draft notes créées) : une publication interrompue reprend sans doublon. Plages locales : jamais publiées (`--dry-run` seulement) | La review apparaît d'un coup comme dans l'interface web ; aucun envoi sans confirmation (`y` dans la TUI, `--yes` en CLI). |
| Commentaires dans la TUI | Vues Diff et Stack : `c` sur une ligne (sur un brouillon : l'éditer ; sur une discussion : y répondre), `V` puis `c` pour une plage, `C` pour le fichier ; éditeur `Ctrl-s` / `Alt-Entrée` pour enregistrer, `Entrée` nouvelle ligne, `Esc` deux fois si le texte a changé. Brouillons (jaune) et discussions GitLab (auteur, résolue ou non, réponses) affichés sous leur ligne ; discussions hors du diff sous l'en-tête du fichier. `P` : panneau Review (commentaire global `S`, brouillons avec `e` / `d`, discussions, `p` publier avec le détail des requêtes, `J` le JSON exact). Discussions, version et brouillons en attente chargés en arrière-plan à l'ouverture d'une MR | Une review complète sans navigateur : lire les discussions, commenter, répondre, publier. |
| Raffinement LSP | Après le graphe tree-sitter, en arrière-plan, sur le worktree : serveurs lancés par le moteur (`lsp`, client JSON-RPC stdio écrit à la main avec `serde_json`). Seulement autour des callables modifiés : `references` sur chacun (appelants confirmés ou ajoutés si l'occurrence est un appel), `definition` aux sites des arêtes `calls` incertaines (< 1) qui les touchent et aux appels qu'ils font. Confirmée → 1,0 + `Edge::lsp` ; contredite (autre cible du dépôt, ou bibliothèque) → supprimée, sauf si confirmée ailleurs ; pas de réponse → inchangée. Arêtes `tests` alignées sur les `calls` | Précision là où la review regarde, pour un coût borné. Le graphe heuristique reste utilisable tout du long. |
| Serveurs LSP | `[lsp.java]` jdtls (`-data {data}`, prêt sur `language/status ServiceReady`, métadonnées Eclipse hors du projet) ; `[lsp.kotlin]` kotlin-language-server puis kotlin-lsp de JetBrains (prêt quand les `$/progress` sont finis) ; `[lsp.typescript]` typescript-language-server (`tsserver` du projet, sinon le global), sinon `tsc --lsp --stdio` de TypeScript ≥ 7. `command` / `args` / `env` / `enabled` par langage, `{data}` = `.git/survol/lsp/<serveur>-<hash>` ; `doctor` dit lesquels sont installés | Des valeurs par défaut qui marchent avec Homebrew / npm ; `env` pour un JDK différent (kotlin-language-server ne démarre pas sur JDK 25). |
| Budget LSP | `[lsp] budget_secs` (60) pour tout le raffinement, démarrage et indexation compris ; `request_timeout_secs` (10) par requête ; un thread par serveur. Serveur absent, en échec ou pas prêt à temps → statut dans `Graph::lsp_stats()`, arêtes inchangées. Cache `.git/survol/cache/<head>/graph-lsp.json` (clé : clé du graphe, `LSP_VERSION`, serveurs résolus), écrit seulement si un serveur a répondu | Jamais bloquant : un premier lancement lent (jdtls, Gradle) se termine plus vite ensuite grâce au répertoire `{data}` conservé. |
| LSP dans la TUI | Lancé quand le graphe et le worktree sont prêts ; en-tête `⟳ LSP: refining 12/40` (ou l'état d'indexation), puis `LSP ✓n` et le graphe raffiné remplace l'autre (`set_graph`) ; `LSP ✗` si aucun serveur n'a pu répondre. `--no-lsp` ou `[lsp] enabled = false` | Changement minimal de la TUI : un statut et un échange de graphe. |

## 4. Les trois vues

On passe d'une vue à l'autre d'une touche. Les trois partagent le même état de review (ce qui est « compris / validé »).

### 4.1 Vue Diff

- Le diff classique : flux continu de tous les fichiers (style GitHub) et/ou fichier par fichier, avec une sidebar.
- Unifié ou côte à côte, avec coloration syntaxique.
- Marquage « relu » par fichier et par hunk, persistant entre les sessions et lié au SHA de la MR. Si un nouveau commit arrive, seuls les hunks modifiés repassent « à relire ».
- Commentaires ligne, plage ou fichier, en brouillon, puis publication vers GitLab.

### 4.2 Vue Regroupement (« Stack »)

- Le LLM regroupe les hunks par **capacité fonctionnelle**, puis par **couche technique** à l'intérieur de chaque groupe.
  - Exemple : « Gestion des commandes » → API / service / persistance / événements.
- Chaque groupe comporte :
  - un titre ;
  - une explication **fonctionnelle** en 2–3 lignes (ce que ça fait côté métier) ;
  - la liste des briques techniques et des hunks concernés.
- Les groupes sont **ordonnés pour la lecture** : les fondations d'abord (modèles, contrats), puis ce qui les utilise, puis les points d'entrée. L'ordre est calculé par **tri topologique sur le graphe réel**, pas laissé à l'intuition du LLM.
- Chaque groupe peut être validé en bloc.
- Groupe spécial **« mécanique / bruit »** : lockfiles, fichiers générés, renommages, formatage pur. Il est détecté de façon déterministe, sans LLM, et se valide d'un coup.

### 4.3 Vue Graphe / Architecture

C'est la vue centrale pour juger l'ensemble.

- **Carte des modules** : les modules et packages, avec le sens des dépendances. Elle permet de voir si les couches sont respectées ou si tout dépend de tout.
- **Flux de bout en bout** : du point d'entrée (endpoint HTTP, consumer, job, commande CLI) jusqu'à la persistance ou aux appels externes.
- **Points de branchement** : configuration, injection de dépendances, frontières (API exposées, messages, base de données, services externes).
- **Vue d'un symbole** : pour une fonction ou une classe, ses appelants, ce qu'elle appelle, les tests qui l'exercent. Le code modifié et le code non modifié sont distingués visuellement.
- Présentation dans le terminal : un **arbre navigable** (nœuds dépliables) plutôt qu'un dessin ASCII. Export **Mermaid** en option pour une vue globale hors terminal.
- `Entrée` sur un nœud ouvre le code à la bonne ligne dans Neovim, y compris dans un fichier non modifié.
- Question au LLM depuis un nœud (« c'est quoi ce composant ? », « pourquoi ça passe par là ? »). La réponse contient des liens vers le code, sur lesquels on peut sauter.

Croquis de la vue symbole :

```
┌ OrderService.create(cmd)  [modifié] ────────────────────────────────────────┐
│ Appelé par (3)                           │ api/OrderController.java:42      │
│  ▸ api/OrderController.java:42   modifié │   @PostMapping("/orders")        │
│  ▸ jobs/ImportJob.java:118       intact  │   public Order post(...) {       │
│  ▸ admin/BulkService.java:56     intact  │ >   return service.create(cmd);  │
│ Appelle (4)                              │   }                              │
│  ▸ OrderRepository.save          modifié │                                  │
│  ▸ PaymentClient.authorize       intact  │                                  │
│ Tests (2)                                │                                  │
│  ▸ OrderServiceTest.create_ok            │                                  │
│ [e] ouvrir dans nvim  [?] demander au LLM  [Tab] vue suivante                │
└──────────────────────────────────────────┴──────────────────────────────────┘
```

## 5. Architecture

```
┌──────────────────────┐     JSON      ┌──────────────────────┐
│ survol-core (moteur) │ ────────────▶ │ survol-tui (ratatui) │
│  - forge GitLab      │               │  - vue Diff          │
│  - git / worktree    │               │  - vue Stack         │
│  - parsing du diff   │               │  - vue Graphe        │
│  - tree-sitter index │               │  - état de review    │
│  - graphe            │               │  - commentaires      │
│  - LLM (groupes...)  │               └──────────┬───────────┘
│  - cache             │                          │ ouvrir fichier:ligne
└──────────────────────┘                          ▼
                                       ┌──────────────────────┐
                                       │  Neovim parent       │
                                       │  (nvim --server …)   │
                                       │  + plugin survol.nvim│
                                       └──────────────────────┘
```

Workspace Cargo suggéré :

```
survol/
  crates/
    survol-core/      # bibliothèque : modèle, forge, git, diff, index, graphe, llm, cache
    survol-cli/       # binaire : sous-commandes JSON (fetch, analyze, group, graph, publish)
    survol-tui/       # binaire : interface ratatui (utilise survol-core directement)
  nvim/
    lua/survol/init.lua   # plugin minimal
  docs/
  HANDOFF.md
```

La TUI peut appeler `survol-core` directement en bibliothèque. La CLI JSON sert aux tests, au debug, aux scripts et à d'éventuels autres clients.

### 5.1 Modèle de données (première version)

```rust
MergeRequest { project, iid, title, description, base_sha, start_sha, head_sha, web_url }
FileChange   { path, old_path, status /* added|modified|deleted|renamed */, language, is_generated }
Hunk         { id, file, old_range, new_range, lines, content_hash }
Symbol       { id, name, kind /* fn|method|class|module */, file, range, changed: bool }
Edge         { from: SymbolId, to: SymbolId, kind /* calls|imports|inherits|tests|configures */, confidence }
Group        { id, title, summary, layers: Vec<Layer>, order, hunk_ids }
Layer        { name /* api|service|persistence|… */, hunk_ids }
ReviewState  { head_sha, reviewed_hunks, reviewed_groups, draft_comments }
```

Invariant clé : **chaque hunk appartient à exactement un groupe** (le groupe « mécanique » compris). Il est vérifié en code après chaque réponse du LLM.

### 5.2 Pipeline du moteur

1. **Fetch MR** : métadonnées et SHAs (base, start, head) via l'API GitLab.
2. **Checkout** : `git fetch origin refs/merge-requests/<iid>/head`, puis création d'un `git worktree` dédié au SHA head, avec la base disponible pour le diff.
3. **Diff** : `git diff -M base...head` en local (plus fiable et complet que l'API sur les grosses MR), parsé en `FileChange` et `Hunk`.
4. **Tri mécanique** : globs (lockfiles, `generated/`, `*.min.*`, etc., configurables), détection des renommages purs et des changements uniquement de formatage (diff structurel, en option via difftastic).
5. **Index tree-sitter** du worktree : définitions et références, via des requêtes de type `tags.scm` par langage.
6. **Mapping hunk ↔ symbole** : chaque hunk est rattaché au symbole qui l'englobe, ce qui produit la liste des symboles modifiés.
7. **Graphe** : résolution des références en arêtes. Résolution par nom et par imports au départ (heuristique, avec un champ `confidence`), raffinée ensuite par le LSP ✅ (`lsp`, voir §3 « Raffinement LSP »).
   - **Règles Spring (priorité v1)** ✅ (règle `spring`, `graph/rules/spring.rs`), car une bonne partie du branchement y est implicite :
     - points d'entrée ✅ : `@RestController` / `@Controller` + `@*Mapping` (préfixe de classe, constantes, méthodes HTTP, mapping hérité d'une interface, opérations OpenAPI implémentées par un contrôleur d'`*Api` généré), `@KafkaListener`, `@RabbitListener`, `@JmsListener`, `@SqsListener`, `@Scheduled`, `@EventListener`, `ApplicationListener`, `CommandLineRunner` / `ApplicationRunner`, `main` ;
     - composants ✅ : `@Service`, `@Component`, `@Repository`, `@Configuration` + `@Bean` (nom de bean, type produit) ;
     - injection ✅ : par constructeur (le cas par défaut), constructeur primaire Kotlin, Lombok, `@Autowired`, `@Qualifier`, `@Primary`. Résolution interface → implémentation(s) : arête `injects` vers l'implémentation, avec une `confidence` plus basse s'il y en a plusieurs ;
     - persistance ✅ : interfaces étendant `JpaRepository` / `CrudRepository`… (→ entité par l'argument générique), `@Entity` / `@Document` / `@Table` ;
     - appels externes ✅ : `@FeignClient` (rôle `external`, liés aux endpoints de l'index), `RestTemplate`, `WebClient`, `RestClient` (tag `external`, URL liée aux endpoints) ;
     - configuration ✅ : `application*.yml|properties` ↔ `@Value` / `@ConfigurationProperties` / placeholders `@FeignClient` (même module préféré, profils en tag) ;
     - événements ✅ : `publishEvent(X)` → listeners de `X` ou d'un supertype (arête `publishes`).
     - Limites : injection de collections (`List<T>`) et `ObjectProvider<T>` non résolue (types génériques réduits), `@Profile` / `@Conditional*` ignorés (toutes les implémentations comptent), routes fonctionnelles WebFlux (`RouterFunction`) non lues.
   - **Angular (priorité v1 côté front)** ✅ (règles `angular` et `http-link`) :
     - points d'entrée ✅ : configuration des routes (tout objet littéral en forme de route : `Routes`, `provideRouter`, `RouterModule.forRoot/forChild`), chemins des routes parentes et préfixes du lazy loading (`loadComponent`, `loadChildren`, y compris l'ancienne forme `'./x#Module'`) ;
     - composants ✅ : `@Component` (selector, `templateUrl` / `template`, `imports` des composants standalone), `@NgModule` (declarations, imports, providers, bootstrap), `@Directive`, `@Pipe` ;
     - templates ✅ : lien selector → composant pour les balises utilisées dans les `.html` et les templates en ligne, par une analyse HTML simple (balises contenant un `-`). `tree-sitter-angular` écartée : grammaire communautaire à compiler en plus pour un gain faible ici. Limite : sélecteurs d'attribut (`[appX]`) ignorés ;
     - injection ✅ : `@Injectable` (`providedIn`), injection par constructeur (`@Inject(TOKEN)` compris) et fonction `inject()`, `InjectionToken`, providers `useClass` / `useExisting` ;
     - données ✅ : services utilisant `HttpClient` (et `axios`, `fetch`), avec la **liaison front ↔ back** : l'URL et la méthode HTTP appelées sont rapprochées des `@*Mapping` Spring. Arête `http_calls` (heuristique, `confidence` affichée). C'est une fonctionnalité clé pour voir un flux de bout en bout, de l'écran jusqu'à la base. Vérifié sur spring-petclinic-angular + spring-petclinic-rest (API générée depuis `openapi.yml`, context-path `/petclinic`) : les 31 appels HTTP des 6 services liés au bon endpoint (0,9) ; sur spring-petclinic-microservices, 7 appels `WebClient` inter-services liés ;
     - configuration ✅ : `environment*.ts` (arête `configures` vers le code qui lit `environment.*`, variantes à 0,6).
     - Limites : URL construite dans une autre méthode ou passée en paramètre → joker (lien plus faible ou absent), `HttpClient.request(...)` non lu.
   - **TypeScript / JavaScript générique** : imports ES / CommonJS, exports, appels.
8. **Regroupement LLM** : entrée = hunks (compressés) + symboles + arêtes. Sortie = JSON validé par schéma, puis contrôle d'invariants et nouvelle tentative si besoin.
9. **Ordonnancement** : tri topologique des groupes selon les arêtes entre eux.
10. **Cache** : dans `.git/survol/<head_sha>/` (index, graphe, groupes, explications). Invalidation incrémentale par `content_hash` de hunk.

### 5.3 Couche LLM

- **Fournisseur v1 : CLI Claude Code** :
  - invocation : `claude -p --output-format json --model <modèle>`, avec le prompt sur stdin, lancée depuis le worktree de la MR ;
  - l'authentification est entièrement déléguée au CLI ;
  - modèle configurable : un modèle rapide pour le regroupement, un modèle plus fort pour les questions ;
  - détection au démarrage (`survol-cli doctor`) : `claude` présent et authentifié ;
  - le tout derrière un trait `LlmProvider`, pour pouvoir ajouter un autre fournisseur plus tard sans toucher au reste.
- **Règles** :
  - Le LLM reçoit des identifiants (hunk_id, symbol_id) et doit répondre avec ces identifiants. Il ne cite jamais de code qu'on ne lui a pas fourni.
  - Toute sortie est en JSON, validée par schéma. En cas d'échec : une nouvelle tentative avec l'erreur, puis repli sur un regroupement par répertoire ou module.
  - Il ne crée aucune arête du graphe.
  - Grosses MR : découpage par module, regroupement par module, puis fusion. Budget de tokens configurable.
- **Prompts versionnés** dans le dépôt (`crates/survol-core/prompts/`). Un fichier de consignes projet optionnel (`.survol/instructions.md`) est injecté, par exemple pour décrire les conventions d'architecture de l'équipe.

### 5.4 Intégration GitLab

- **Instance auto-hébergée** : `GITLAB_HOST` obligatoire, aucune URL gitlab.com par défaut, support d'une CA d'entreprise (`ca_cert` dans la config, ou magasin système).
- Authentification : réutiliser la config de `glab` pour cet hôte si elle existe, sinon `GITLAB_TOKEN`.
- Vérifier la version de l'instance au démarrage (`GET /version`) et désactiver proprement les fonctionnalités absentes (ex. brouillons de review sur une instance trop ancienne).
- Endpoints utiles :
  - `GET /projects/:id/merge_requests/:iid` (métadonnées)
  - `GET /projects/:id/merge_requests/:iid/versions` (base / start / head SHA)
  - `POST /projects/:id/merge_requests/:iid/draft_notes` (commentaires en brouillon, avec `position`)
  - `POST /projects/:id/merge_requests/:iid/draft_notes/bulk_publish` (publication de la review)
  - `GET /projects/:id/merge_requests/:iid/discussions` (afficher les discussions existantes)
- Une `position` de commentaire inline exige `base_sha`, `start_sha`, `head_sha`, `old_path` / `new_path` et `old_line` / `new_line`. C'est le point technique le plus délicat : prévoir des tests dédiés.
- Garder un trait `Forge` pour pouvoir ajouter GitHub plus tard.

### 5.5 Intégration Neovim

- Plugin Lua minimal (`survol.nvim`) :
  - `:Survol [mr]` ouvre `survol-tui` dans un terminal flottant. `mr` peut être un numéro, une URL, ou vide (MR de la branche courante) ;
  - expose `$NVIM` à la TUI.
- Depuis la TUI, l'action « ouvrir » exécute `nvim --server "$NVIM" --remote-send` (ou `--remote`) pour ouvrir `fichier:ligne` dans le Neovim parent, en option dans un nouvel onglet ou une nouvelle fenêtre.
- Hors de Neovim, la TUI lance `$EDITOR +ligne fichier`.
- Terminal persistant : ouvrir un fichier masque le flottant sans tuer survol ; `:SurvolBack` / `:SurvolToggle` le réaffichent au même endroit (voir étape 4).

## 6. État de l'art (septembre 2026) et ce qu'on en retient

| Outil | Ce que c'est | À retenir |
|---|---|---|
| **tuicr** (Rust, MIT) | TUI de review avec raccourcis vim, diff continu, suivi relu par hunk, publication vers GitLab | Excellente référence, voire base, pour la **vue Diff** et la publication. |
| **hunk** (TS / OpenTUI) | Visualiseur de diff orienté review de code d'agent, annotations IA en ligne | Idées d'interface. Écosystème d'extensions (exclusion de fichiers, etc.). |
| **leanreview** (Go) | TUI de review multi-forge qui s'appuie sur git, gh et glab | Bon découpage « la forge reste à la forge ». |
| **gitlab.nvim** | Client GitLab dans Neovim, s'appuie sur diffview | Référence pour la gestion des positions de commentaires GitLab. |
| **CodeRabbit Change Stack** | Cohortes et couches ordonnées avec résumés, dans le navigateur | Le modèle de la **vue Stack**. Leur mise en garde : des cohortes fausses ou mal ordonnées sont pires qu'un diff à plat, d'où la validation par invariants et l'ordre par graphe. |
| **PR-Agent** (open source) | `/describe`, `/review` sur GitLab, CLI | Idées pour la compression des grosses MR. Peut coexister en CI pour la chasse aux bugs. |
| **semantic-diff** | Regroupement des hunks par intention via un CLI d'agent | Modèle d'appel des CLI d'agents avec repli. A finalement quitté le terminal pour une interface web. |
| **wonk**, **code-review-graph**, **code-graph** | Graphes de code tree-sitter : callers, callees, impact, flows | Modèle de commandes pour le **moteur de graphe**. Assument une précision moindre que le LSP. |
| **CodeSee Review Maps** (web, GitHub) | Review d'une PR sous forme de carte : connexions entre fichiers, fichiers non modifiés mais liés (imports dans les deux sens) affichés, review par blocs logiques | Référence la plus proche de la **vue Graphe** et de l'idée « voir le code non modifié lié ». Valide le concept, donne des repères d'interface. |
| **lazydiff** (Rust) | TUI de review de diffs git et de PR GitHub, avec vue des changements sémantiques | Voisin direct côté interface. GitHub uniquement, sans regroupement LLM ni graphe d'architecture. |
| **gitlab-reviewer** (Go) | TUI qui liste les MR GitLab, fait un checkout en worktree détaché et lance le CLI Claude Code sur la MR, avec découpage automatique des grosses MR | Même socle technique (GitLab + worktree + Claude Code) : bonne référence. Mais il produit des suggestions de commentaires (bugs, sécurité, style), donc de la chasse aux bugs, le contraire de notre positionnement sur la compréhension. |
| **ChangeMap** (VS Code, JS/TS) | Analyse d'impact locale : ce qu'un changement affecte (fichiers, fonctions, tests), avec les éléments qui le justifient | Même logique que notre vue d'impact, mais dans l'IDE et hors review de MR. |

**Positionnement de `survol`** : aucun outil ne combine aujourd'hui regroupement façon Change Stack, graphe d'architecture navigable, terminal / Neovim et MR GitLab, avec pour objectif la compréhension plutôt que la détection de bugs.

## 7. Roadmap

Chaque étape doit produire un outil utilisable sur une vraie MR.

### Étape 0 — Squelette ✅
- Workspace Cargo, CI (fmt, clippy, tests), config (`~/.config/survol/config.toml` + `.survol/` dans le projet).
- **Critère** : `survol --help` et `survol-cli doctor` (vérifie git, glab ou token, nvim, CLI LLM).

### Étape 1 — Vue Diff sur une vraie MR ✅ (validée sur une plage locale de 869 fichiers : affichage en ~1 s ; reste à valider sur une vraie MR GitLab)
- Fetch de la MR, worktree, diff local, TUI diff (flux continu + sidebar, raccourcis vim), état « relu » persistant.
- **Critère** : ouvrir une MR de plus de 500 fichiers en moins de 5 s (hors fetch réseau) et naviguer de façon fluide.

### Étape 2 — Vue Stack ✅ (reste à valider sur une vraie MR GitLab)
- Tri mécanique déterministe, regroupement LLM, validation des invariants, explications par groupe, validation par groupe.
- Moteur : `llm` (trait `LlmProvider`, `ClaudeCli`), `mechanical` (généré, blancs, fichiers sans hunk), `group` (compression, découpage par module + fusion, validation, nouvelle tentative, repli, ordre provisoire, cache), `review::group`, `survol-cli group [--no-cache] [--no-llm]`. Smoke test sur whisper.cpp `HEAD~15..HEAD` (104 fichiers, 683 hunks) : 16 groupes pertinents, 100 % des hunks attribués après une nouvelle tentative, ~60 s, réouverture depuis le cache en 0,25 s.
- TUI : vues Diff et Stack (`Tab` / `1` `2`), regroupement en arrière-plan au lancement, arbre groupes → couches → hunks à gauche, résumé + diff du nœud à droite (même rendu que la vue Diff), validation par `space`, saut vers la vue Diff (`Enter` / `gd`), `R` pour régénérer (confirmation), `--no-llm`. Testé sur whisper.cpp : 14 groupes LLM en ~30 s, réouverture instantanée depuis le cache.
- **Critère** : sur une MR réelle, 100 % des hunks sont attribués, les groupes sont jugés pertinents par le relecteur, et tout est mis en cache (réouverture instantanée).

### Étape 3 — Graphe ✅
- Index tree-sitter (langages prioritaires, voir §9), mapping hunk ↔ symbole, arêtes calls/imports/tests, vue symbole, carte des modules, ordre des groupes par tri topologique.
- Moteur : `index` (requêtes `queries/*.scm`, extraction, cache par blob), `graph` (symboles, résolution heuristique avec confiance, `callers` / `callees` / `tests_of` / `symbol_at` / `find` / `module_map`, trait `FrameworkRule`), `group::order_groups_with_graph`, `review::build_graph` (cache `graph.json`), `survol-cli graph [--symbol NAME] [--modules] [--no-cache]`.
- Critère vérifié sur des clones publics (branche locale modifiant une méthode) : spring-petclinic (Java) `Owner.addPet` → appelants `PetController.initCreationForm/processCreationForm/updatePetDetails` (0,85) + 8 tests ; spring-petclinic-kotlin `Owner.addPet` → 3 méthodes de `PetController` (0,85), `Pet.addVisit` → `VisitController.loadPetWithVisit` (0,5) ; spring-petclinic-angular `OwnerService.getOwnerById` → 6 composants (0,9) + le spec. Limites connues : surcharges de même arité (`getPet(Integer)` / `getPet(String)` → 0,45 chacune), types inférés depuis un appel (`val pet = repo.findById(id)`) non résolus, appels depuis les callbacks `describe`/`it` rattachés au fichier, DI et liens HTTP couverts depuis par les règles Spring / Angular (voir §5.2).
- Vue Graphe dans la TUI ✅ (vue 3) : graphe construit en arrière-plan au lancement (puis groupes Stack réordonnés par `order_with_graph`) ; modes « symboles modifiés » (appelants, dont ceux des fichiers intacts), « symbole » (arbre Appelé par / Appelle / Tests puis tout autre type d'arête, nœuds dépliables récursivement, `Entrée` = nouvelle racine, pile de retour `Backspace` / `Ctrl-o`, aperçu du code à droite) et « carte des modules » (export Mermaid `x`, ou `survol-cli graph --mermaid`) ; `e` ouvre la ligne du nœud (fichiers non modifiés compris), `gd` / `gs` font l'aller-retour avec les vues Diff et Stack. Vérifié en tmux sur les trois clones publics.
- Règles Spring, Angular et liaison front ↔ back ✅ (`spring`, `angular`, `http-link`, voir §5.2). Limites : injection de collections (`List<T>`), `@Profile` / `@Conditional*`, sélecteurs d'attribut Angular, URL construites dans une autre méthode, routes WebFlux fonctionnelles.
- **Critère** : pour une méthode modifiée, liste correcte de ses appelants dans des fichiers non modifiés sur un projet réel.

### Étape 4 — Neovim ✅
- Plugin `survol.nvim`, saut vers `fichier:ligne` dans le Neovim parent, retour à la TUI.
- Fait : terminal flottant **persistant** (le job survol continue quand on saute vers le code ; `:Survol` / `:SurvolToggle` / `:SurvolBack` réaffichent le même terminal ; nouveau job seulement si aucun ne tourne ou si la cible change), `open_mode` (`edit` dans la fenêtre d'origine, `tab`, `split`, `vsplit`), réutilisation d'une fenêtre qui montre déjà le fichier, complétion des options et branches, `:checkhealth survol`. La TUI appelle `require'survol'.open_file(path, line, col)` via `nvim --server $NVIM --remote-expr "luaeval(…, [path, line, col])"` (chemin passé en chaîne Vim : espaces et guillemets sans échappement Lua) ; réponse `ok` / `error: …` (affichée dans la barre d'état) / `noplugin` → repli `:tabedit` par `--remote-send`. Hors Neovim : `$VISUAL` / `$EDITOR` avec suspension / reprise du terminal. Redessin complet sur `Resize` / `FocusGained`.
- Tests : unitaires Rust (expression, échappement, réponses), tests Neovim headless `nvim/tests/survol_spec.lua` (modes, ligne/colonne, chemin avec espaces et guillemets depuis le mode terminal via RPC réel, masquer/réafficher = même job et même buffer, repli `noplugin`), job CI dédié (Neovim 0.11.4).
- Critère vérifié en tmux sur spring-petclinic `main..graph-check --no-llm` : vue Diff curseur sur la ligne 130 supprimée → `e` ouvre `Owner.java:129` dans la fenêtre d'origine, flottant masqué ; `:Survol` → flottant identique à la capture d'avant (curseur au même endroit, seul le message d'état change). Idem vue Graphe (`Owner.getPet:126`). Limites : pas de colonne transmise par les vues (ligne seulement) ; le LSP n'est pas testé en CI (Neovim sans config).

### Étape 5 — Questions et commentaires
- Question au LLM depuis un nœud ou un groupe, avec réponse contenant des liens navigables. Commentaires en brouillon et publication de la review sur GitLab.
- Questions ✅ : moteur `ask` (contexte, validation des références, cache, historique), `survol-cli ask [TARGET] --symbol NAME|--group N|--hunk ID "question"` (`--prompt` affiche le prompt sans appel), touches `a` / `A` dans la TUI. Vérifié sur spring-petclinic `main..graph-check` : « What does this component do? » sur `Owner.addPet` → réponse avec 8 références, toutes valides (dont une ligne retirée `Owner.java:98 (old)` et les appels de `PetController`), sauts vers la vue Graphe et le Diff.
- Commentaires ✅ : moteur `comments` (brouillons ancrés, positions, plan et publication avec reprise), trait `Forge` étendu (`discussions` paginées, `draft_notes`, `create_draft_note`, `publish_drafts`, `post_comment`), `survol-cli comments [TARGET]` et `survol-cli publish [TARGET] --dry-run | --yes`, touches `c` / `V` / `C` / `P` dans la TUI. Positions testées contre un faux serveur HTTP (JSON exact envoyé) ; vérifié en tmux sur la plage locale (brouillons, `--dry-run`) et de bout en bout contre un faux GitLab local (discussions affichées, réponse, commentaire global, publication `draft_notes` + `bulk_publish`). Reste à valider sur la vraie instance : positions acceptées (surtout `line_range` et `position_type: file`), seuils de version, publication.
- **Critère** : une review complète faite sans ouvrir le navigateur.

### Étape 6 — Précision et confort ✅
- Raffinement des arêtes par LSP (clients LSP lancés par le moteur), flux de bout en bout depuis les points d'entrée, export Mermaid, vue avant / après d'un flux quand c'est pertinent.
- Raffinement LSP ✅ : moteur `lsp` (client JSON-RPC, serveurs, plan et application des réponses, budget, cache), `review::refine_graph`, `survol-cli graph --lsp` (compte des arêtes `calls`/`tests` par tranche de confiance avant / après), `doctor` (serveurs installés), TUI (statut dans l'en-tête, graphe remplacé à la fin). Tests unitaires avec un faux serveur en mémoire ; test réel jdtls `#[ignore]`. Mesuré sur les clones publics (branches `graph-check`) :
  - spring-petclinic (jdtls) : `Owner.getPet(Integer)` / `getPet(String)` départagés : `getPet(Integer)` a 7 appelants à 1,0 (`addVisit`, `findPet`, `updatePetDetails`, `loadPetWithVisit` + 3 tests) au lieu de 11 à 0,43 partagés avec `getPet(String)` ; arêtes < 0,5 : 42 → 6, 39 confirmées par LSP, 18 supprimées ; démarrage 1,3–3,3 s, indexation 0,3 s, total 3,5–7 s.
  - spring-petclinic-kotlin (kotlin-language-server, JDK 21) : `Pet.addVisit` ← `VisitController.loadPetWithVisit` (`val pet = pets.findById(petId)`, type inféré) 0,5 → 1,0, test 0,3 → 1,0 ; `Owner.addPet` ← 3 méthodes de `PetController` 0,85 → 1,0. Premier lancement 78 s (résolution du classpath Gradle pendant `initialize` : dépasse le budget par défaut), ensuite 30 s.
  - spring-petclinic-angular (`tsc --lsp` de TypeScript 7, ou typescript-language-server + TypeScript 5) : `OwnerService.getOwnerById` ← 6 composants + le spec, 0,9 / 0,5 → 1,0 ; 0,7–2,5 s.
  - spring-framework (9 611 fichiers, jdtls) : `StringUtils.hasText(String)` : 386 des 396 appelants confirmés, 389 arêtes vers la surcharge `hasText(CharSequence)` supprimées ; 401 requêtes en 17 s (démarrage 1,6 s, indexation 4,6 s).
  - Limites : kotlin-lsp (JetBrains) bloqué au lancement sur ce poste (processus arrêté avant `main`, sans doute la politique de sécurité macOS), donc kotlin-language-server par défaut ; les références de méthode (`Foo::bar`) restent souvent incertaines ; jdtls n'est prêt qu'une fois le projet importé (un projet Gradle inconnu peut dépasser le budget au premier lancement) ; seules les arêtes `calls` / `tests` autour des callables modifiés sont vérifiées (dispatch par interface, injections et liens HTTP inchangés).
- Flux ✅ : moteur `flows` (points d'entrée dont le flux atteint un symbole modifié, par parcours inverse ; arbre en profondeur par `calls`, implémentations d'interface restreintes aux `injects`, `http_calls` front → back, `publishes`, méthodes et `routes` d'un composant routé ; terminaux persistance / entité / appel externe / événement ; limites de profondeur, de branches, de pas et de confiance cumulée ; cycles et répétitions non dépliés ; branches sans changement ni terminal élaguées). Avant / après : graphe de la révision de base (`Index::build_revision`, cache par blob partagé ; complet jusqu'à 5 000 fichiers, sinon voisinage des flux), comparaison par id (pas ajoutés / retirés / déroutés, appels externes et accès persistance nouveaux ou disparus, entrées nouvelles ou supprimées), export Mermaid. `survol-cli flows [TARGET] [--entry NAME] [--mermaid] [--no-base]`, mode « flows » de la vue Graphe (`f` ou `m`, `b` avant / après / fusion, `x` Mermaid). Vérifié en tmux sur petclinic-monorepo (branche locale : écran d'édition de visite rerouté vers un nouvel endpoint `/details`, notification REST ajoutée à `saveVisit`) : 6 flux impactés, écran → service Angular → HTTP → contrôleur → service → dépôts → entités. Limites : liens HTTP incertains (< 0,5) traités comme appels externes, méthodes héritées de Spring Data (`findById` non déclaré) sans arête, fusion avant / après alignée par id (un pas répété n'est pas redéplié).

## 8. Risques

| Risque | Parade |
|---|---|
| Regroupement LLM incohérent ou instable | Invariants vérifiés en code, JSON avec schéma, ordre calculé par le graphe, repli par module, cache par SHA. |
| MR trop grosse pour le contexte du LLM | Tri mécanique d'abord, compression des hunks, découpage par module puis fusion. |
| Graphe tree-sitter imprécis (surcharges, DI, réflexion, frameworks) | Champ `confidence` affiché, raffinement LSP ✅ (étape 6) autour des symboles modifiés, règles spécifiques par framework (ex. annotations Spring). |
| Positions de commentaires GitLab fragiles | Tests d'intégration dédiés, s'inspirer de gitlab.nvim et tuicr. |
| Coût et latence LLM | Modèle rapide pour le regroupement, cache agressif, calcul en arrière-plan pendant que la vue Diff est déjà utilisable. |
| Confidentialité du code | Fournisseur LLM configurable, y compris un modèle auto-hébergé. Aucune télémétrie. |

## 9. Questions ouvertes

Tranchées : langages (Java / Kotlin Spring, puis TS / JS avec **Angular**), LLM (CLI Claude Code), GitLab (auto-hébergé), TUI (ratatui), nom (`survol`). Voir §3.

1. **Version de l'instance GitLab** auto-hébergée (pour les brouillons de review et les endpoints disponibles). Seuils supposés : draft notes ≥ 15.10, commentaire de fichier ≥ 16.4 (`forge::DRAFT_NOTES_SINCE`, `FILE_COMMENTS_SINCE`), à confirmer sur l'instance.

## 10. Pour démarrer avec Claude Code

1. Créer un dépôt vide, y déposer ce `HANDOFF.md`, lancer Claude Code à la racine.
2. Premier message suggéré :
   > Lis HANDOFF.md. Réalise l'étape 0 puis l'étape 1. Avant de coder, propose-moi la structure du workspace et les crates que tu comptes utiliser (ratatui, crossterm, git2 ou appels git, reqwest, serde, etc.), puis attends ma validation.
3. Garder ce document à jour. Les décisions nouvelles vont en §3, les questions tranchées sortent du §9.
4. Tenir à disposition une **vraie MR de référence** (volumineuse) pour tester chaque étape.
