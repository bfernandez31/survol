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
| Cache des groupes | `.git/survol/cache/<head_sha>/groups.json`, clé = hash des `content_hash` + classement mécanique + `PROMPT_VERSION` + modèle + budget + consignes projet. Les replis complets par répertoire ne sont pas mis en cache | Réouverture sans appel LLM ; un nouvel essai LLM au prochain lancement si le LLM était indisponible. |
| Effort de raisonnement | `[llm] group_effort = "low"` par défaut (`--effort`) | Mesuré sur whisper.cpp (683 hunks, sonnet) : effort par défaut ≈ 90 % de tokens de réflexion, ~5 min et ~0,65 $ par appel ; `low` : ~25 s, ~0,35 $, groupes un peu plus gros. `medium` possible pour des groupes plus fins. |
| Debug LLM | `SURVOL_LLM_LOG=<dir>` garde chaque prompt et réponse brute | Diagnostic du coût, de la latence et des prompts. |
| Validation d'un groupe | Pas d'état dédié : `Group::set_reviewed` marque ses hunks (et fichiers sans hunk) dans le `ReviewState` commun | Les trois vues partagent le même état « relu ». |
| Navigation entre vues | `Tab` / `Shift-Tab` et `1` / `2` changent de vue ; `Ctrl-h` / `Ctrl-l` changent de panneau (liste ↔ contenu) | `Tab` est réservé aux vues (3 à terme) ; `Ctrl-h/l` comme les fenêtres Neovim. |
| Regroupement dans la TUI | Lancé en arrière-plan à l'ouverture, cache d'abord ; `R` régénère sans cache après confirmation y/n | La vue Diff reste utilisable pendant l'appel LLM ; pas d'appel payant par erreur. |
| Mode sans LLM | `--no-llm` ou `[llm] enabled = false` : groupe mécanique + regroupement par répertoire, sans cache | Confidentialité, hors ligne, tests. |
| `space` dans la vue Stack | Valide tout le nœud (groupe, couche, hunk). Groupe terminé → replié, passage au groupe suivant non relu ; sinon nœud suivant non relu du même groupe | Valider un groupe d'un coup, sans quitter un groupe à moitié relu. |
| Langue des résumés | Anglais (prompts en anglais) | À rediscuter si le relecteur préfère le français (option de config possible). |

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
7. **Graphe** : résolution des références en arêtes. Résolution par nom et par imports au départ (heuristique, avec un champ `confidence`), raffinée par le LSP plus tard.
   - **Règles Spring (priorité v1)**, car une bonne partie du branchement y est implicite :
     - points d'entrée : `@RestController` / `@Controller` + `@*Mapping`, `@KafkaListener`, `@RabbitListener`, `@JmsListener`, `@Scheduled`, `@EventListener`, `CommandLineRunner` ;
     - composants : `@Service`, `@Component`, `@Repository`, `@Configuration` + `@Bean` ;
     - injection : par constructeur (le cas par défaut), `@Autowired`, `@Qualifier`. Résolution interface → implémentation(s) : arête `injects` vers l'implémentation, avec une `confidence` plus basse s'il y en a plusieurs ;
     - persistance : interfaces étendant `JpaRepository` / `CrudRepository`, `@Entity` ;
     - appels externes : `@FeignClient`, `RestTemplate`, `WebClient` ;
     - configuration : `application*.yml|properties` ↔ `@Value` / `@ConfigurationProperties`.
   - **Angular (priorité v1 côté front)** :
     - points d'entrée : configuration des routes (`Routes`, `provideRouter`, `RouterModule.forRoot/forChild`), y compris le lazy loading (`loadComponent`, `loadChildren`) ;
     - composants : `@Component` (selector, `templateUrl` / `template`, `imports` des composants standalone), `@NgModule` (declarations, imports, providers) ;
     - templates : lien selector → composant pour les balises utilisées dans les `.html`, via la grammaire communautaire `tree-sitter-angular` (à évaluer) ou une analyse HTML simple ;
     - injection : `@Injectable` (`providedIn`), injection par constructeur et fonction `inject()`, `InjectionToken` ;
     - données : services utilisant `HttpClient`, avec la **liaison front ↔ back** : l'URL et la méthode HTTP appelées sont rapprochées des `@*Mapping` Spring. Arête `http_calls` (heuristique, `confidence` affichée). C'est une fonctionnalité clé pour voir un flux de bout en bout, de l'écran jusqu'à la base.
     - configuration : `environment*.ts`.
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
- Plus tard, éventuellement : commande Neovim pour revenir à la TUI au même endroit.

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

### Étape 3 — Graphe
- Index tree-sitter (langages prioritaires, voir §9), mapping hunk ↔ symbole, arêtes calls/imports/tests, vue symbole, carte des modules, ordre des groupes par tri topologique.
- **Critère** : pour une méthode modifiée, liste correcte de ses appelants dans des fichiers non modifiés sur un projet réel.

### Étape 4 — Neovim
- Plugin `survol.nvim`, saut vers `fichier:ligne` dans le Neovim parent, retour à la TUI.
- **Critère** : cycle TUI → Neovim (LSP disponible) → TUI sans perte de position.

### Étape 5 — Questions et commentaires
- Question au LLM depuis un nœud ou un groupe, avec réponse contenant des liens navigables. Commentaires en brouillon et publication de la review sur GitLab.
- **Critère** : une review complète faite sans ouvrir le navigateur.

### Étape 6 — Précision et confort
- Raffinement des arêtes par LSP (clients LSP lancés par le moteur), flux de bout en bout depuis les points d'entrée, export Mermaid, vue avant / après d'un flux quand c'est pertinent.

## 8. Risques

| Risque | Parade |
|---|---|
| Regroupement LLM incohérent ou instable | Invariants vérifiés en code, JSON avec schéma, ordre calculé par le graphe, repli par module, cache par SHA. |
| MR trop grosse pour le contexte du LLM | Tri mécanique d'abord, compression des hunks, découpage par module puis fusion. |
| Graphe tree-sitter imprécis (surcharges, DI, réflexion, frameworks) | Champ `confidence` affiché, raffinement LSP en étape 6, règles spécifiques par framework (ex. annotations Spring) si besoin. |
| Positions de commentaires GitLab fragiles | Tests d'intégration dédiés, s'inspirer de gitlab.nvim et tuicr. |
| Coût et latence LLM | Modèle rapide pour le regroupement, cache agressif, calcul en arrière-plan pendant que la vue Diff est déjà utilisable. |
| Confidentialité du code | Fournisseur LLM configurable, y compris un modèle auto-hébergé. Aucune télémétrie. |

## 9. Questions ouvertes

Tranchées : langages (Java / Kotlin Spring, puis TS / JS avec **Angular**), LLM (CLI Claude Code), GitLab (auto-hébergé), TUI (ratatui), nom (`survol`). Voir §3.

1. **Version de l'instance GitLab** auto-hébergée (pour les brouillons de review et les endpoints disponibles).

## 10. Pour démarrer avec Claude Code

1. Créer un dépôt vide, y déposer ce `HANDOFF.md`, lancer Claude Code à la racine.
2. Premier message suggéré :
   > Lis HANDOFF.md. Réalise l'étape 0 puis l'étape 1. Avant de coder, propose-moi la structure du workspace et les crates que tu comptes utiliser (ratatui, crossterm, git2 ou appels git, reqwest, serde, etc.), puis attends ma validation.
3. Garder ce document à jour. Les décisions nouvelles vont en §3, les questions tranchées sortent du §9.
4. Tenir à disposition une **vraie MR de référence** (volumineuse) pour tester chaque étape.
