//! Framework rules on inline sources.

use crate::graph::{EdgeKind, Graph, Role, SymIdx, build, default_rules};
use crate::index::{Index, Lang, parse_file};

fn graph(files: &[(&str, &str)]) -> Graph {
    let mut v: Vec<_> = files
        .iter()
        .map(|(p, s)| parse_file(p, Lang::from_path(p).expect("indexed path"), s))
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    let index = Index {
        files: v,
        base_files: Vec::new(),
        stats: Default::default(),
    };
    build(&index, &Default::default(), &default_rules())
}

/// The symbol displayed as `name` (`Class.method`, or a file name).
fn sym(g: &Graph, name: &str) -> SymIdx {
    let found: Vec<SymIdx> = (0..g.symbols().len() as SymIdx)
        .filter(|&s| {
            g.display_name(s) == name && g.symbol(s).kind != crate::graph::SymbolKind::Constructor
        })
        .collect();
    assert_eq!(found.len(), 1, "{name}: {found:?}");
    found[0]
}

fn tag<'g>(g: &'g Graph, name: &str, key: &str) -> Option<&'g str> {
    g.symbol(sym(g, name)).tags.get(key).map(String::as_str)
}

/// `from -> to (confidence)` of the edges of `kind`, sorted.
fn edges(g: &Graph, kind: EdgeKind) -> Vec<String> {
    let mut v: Vec<String> = g
        .edges()
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| {
            format!(
                "{} -> {} ({:.2})",
                g.display_name(e.from),
                g.display_name(e.to),
                e.confidence
            )
        })
        .collect();
    v.sort();
    v
}

const OWNER_API: &str = r#"package app.web;
import org.springframework.web.bind.annotation.*;
public interface OwnerApi {
    @GetMapping("/{id}/pets")
    List<Pet> pets(@PathVariable int id);
}
"#;

const OWNER_CONTROLLER: &str = r#"package app.web;
import org.springframework.web.bind.annotation.*;
@RestController
@RequestMapping(Paths.API + "/owners")
public class OwnerController implements OwnerApi {
    @GetMapping("/{id}")
    public Owner show(@PathVariable int id) { return null; }
    @RequestMapping(value = {"", "/"}, method = {RequestMethod.POST, RequestMethod.PUT})
    public Owner save(Owner o) { return o; }
    @Override
    public List<Pet> pets(int id) { return null; }
    public void helper() {}
}
"#;

const PATHS: &str = r#"package app.web;
public final class Paths {
    public static final String API = "/api";
}
"#;

const VET_CONTROLLER_KT: &str = r#"package app.web
@Controller
class VetController(private val vets: VetRepository) {
    @GetMapping("/vets", "/vets.html")
    fun list(): String = "vets"
}
"#;

#[test]
fn spring_endpoints_combine_prefixes_methods_and_inherited_mappings() {
    let g = graph(&[
        ("src/main/java/app/web/OwnerApi.java", OWNER_API),
        (
            "src/main/java/app/web/OwnerController.java",
            OWNER_CONTROLLER,
        ),
        ("src/main/java/app/web/Paths.java", PATHS),
        (
            "src/main/kotlin/app/web/VetController.kt",
            VET_CONTROLLER_KT,
        ),
    ]);
    assert_eq!(
        tag(&g, "OwnerController.show", "http.route"),
        Some("GET /api/owners/{id}")
    );
    assert_eq!(
        tag(&g, "OwnerController.save", "http.route"),
        Some("POST /api/owners, PUT /api/owners")
    );
    // Mapping declared on the implemented interface, prefix of the class.
    assert_eq!(
        tag(&g, "OwnerController.pets", "http.route"),
        Some("GET /api/owners/{id}/pets")
    );
    assert_eq!(tag(&g, "OwnerController.show", "entry"), Some("http"));
    assert!(
        g.symbol(sym(&g, "OwnerController.show"))
            .has_role(Role::EntryPoint)
    );
    assert!(
        !g.symbol(sym(&g, "OwnerController.helper"))
            .has_role(Role::EntryPoint)
    );
    assert!(
        g.symbol(sym(&g, "OwnerController"))
            .has_role(Role::Component)
    );
    assert_eq!(
        tag(&g, "VetController.list", "http.route"),
        Some("GET /vets, GET /vets.html")
    );
}

#[test]
fn spring_listeners_runners_and_main() {
    let g = graph(&[(
        "src/main/java/app/Jobs.java",
        r#"package app;
@SpringBootApplication
public class App {
    public static void main(String[] args) {}
}
@Component
class Jobs {
    @KafkaListener(topics = {"orders", "refunds"}, groupId = "g")
    void onOrder(String m) {}
    @Scheduled(cron = "0 0 * * * *")
    void nightly() {}
    @RabbitListener(queues = "mails")
    void onMail(String m) {}
}
class Init implements CommandLineRunner {
    public void run(String... args) {}
}
"#,
    )]);
    assert_eq!(tag(&g, "App.main", "entry"), Some("main"));
    assert_eq!(tag(&g, "Jobs.onOrder", "entry"), Some("kafka"));
    assert_eq!(
        tag(&g, "Jobs.onOrder", "kafka.topics"),
        Some("orders, refunds")
    );
    assert_eq!(tag(&g, "Jobs.nightly", "schedule"), Some("0 0 * * * *"));
    assert_eq!(tag(&g, "Jobs.onMail", "rabbit.queues"), Some("mails"));
    assert_eq!(tag(&g, "Init.run", "entry"), Some("runner"));
    assert!(g.symbol(sym(&g, "Init")).has_role(Role::EntryPoint));
}

const NOTIFIER: &str = r#"package app.notify;
public interface Notifier { void send(String m); }
"#;
const MAIL: &str = r#"package app.notify;
@Service("mail")
public class MailNotifier implements Notifier { public void send(String m) {} }
"#;
const SMS: &str = r#"package app.notify;
@Service
@Primary
public class SmsNotifier implements Notifier { public void send(String m) {} }
"#;
const CLOCK_CONFIG: &str = r#"package app.notify;
@Configuration
public class ClockConfig {
    @Bean
    public Clock clock(Notifier notifier) { return null; }
}
"#;
const CLOCK: &str = r#"package app.notify;
public interface Clock {}
"#;
const USERS: &str = r#"package app.notify;
@Service
@RequiredArgsConstructor
public class Users {
    private final Notifier notifier;
    private final Clock clock;
    private static final int MAX = 3;
    @Autowired
    @Qualifier("mail")
    private Notifier mailer;
    private final UserRepository repo;
}
"#;
const USER_REPO: &str = r#"package app.notify;
public interface UserRepository extends JpaRepository<User, Long> {}
"#;
const USER: &str = r#"package app.notify;
@Entity
@Table(name = "users")
public class User {}
"#;
const AUDIT_KT: &str = r#"package app.notify
@Service
class Audit(@Qualifier("mail") private val notifier: Notifier, private val repo: UserRepository)
"#;

#[test]
fn spring_injection_resolves_implementations_and_narrows() {
    let g = graph(&[
        ("src/main/java/app/notify/Notifier.java", NOTIFIER),
        ("src/main/java/app/notify/MailNotifier.java", MAIL),
        ("src/main/java/app/notify/SmsNotifier.java", SMS),
        ("src/main/java/app/notify/ClockConfig.java", CLOCK_CONFIG),
        ("src/main/java/app/notify/Clock.java", CLOCK),
        ("src/main/java/app/notify/Users.java", USERS),
        ("src/main/java/app/notify/UserRepository.java", USER_REPO),
        ("src/main/java/app/notify/User.java", USER),
        ("src/main/kotlin/app/notify/Audit.kt", AUDIT_KT),
    ]);
    assert_eq!(
        edges(&g, EdgeKind::Injects),
        [
            // Kotlin primary constructor: `@Qualifier("mail")`, the repository.
            "Audit -> MailNotifier (0.81)",
            "Audit -> UserRepository (0.81)",
            // `@Bean` parameter: two implementations, `@Primary` wins.
            "ClockConfig.clock -> SmsNotifier (0.81)",
            // Lombok final fields: `@Primary`, the `@Bean` producer of
            // `Clock`, the repository; `@Autowired @Qualifier` field.
            "Users -> ClockConfig.clock (0.81)",
            "Users -> MailNotifier (0.81)",
            "Users -> SmsNotifier (0.81)",
            "Users -> UserRepository (0.81)",
        ]
    );
    assert_eq!(tag(&g, "MailNotifier", "spring.bean"), Some("mail"));
    assert_eq!(tag(&g, "ClockConfig.clock", "bean.type"), Some("Clock"));
    let repo = g.symbol(sym(&g, "UserRepository"));
    assert!(repo.has_role(Role::Repository));
    assert_eq!(
        repo.tags.get("persistence.entity").map(String::as_str),
        Some("User")
    );
    let user = g.symbol(sym(&g, "User"));
    assert!(user.has_role(Role::Entity));
    assert_eq!(
        user.tags.get("persistence.table").map(String::as_str),
        Some("users")
    );
    assert!(edges(&g, EdgeKind::Uses).contains(&"UserRepository -> User (0.90)".to_string()));
}

#[test]
fn spring_injection_splits_confidence_without_a_winner() {
    let g = graph(&[
        ("src/main/java/app/notify/Notifier.java", NOTIFIER),
        ("src/main/java/app/notify/MailNotifier.java", MAIL),
        (
            "src/main/java/app/notify/PushNotifier.java",
            "package app.notify;\n@Component\npublic class PushNotifier implements Notifier { public void send(String m) {} }\n",
        ),
        (
            "src/main/java/app/notify/Sender.java",
            "package app.notify;\n@Service\npublic class Sender {\n  Sender(Notifier n) {}\n}\n",
        ),
        (
            "src/main/java/app/notify/Named.java",
            "package app.notify;\n@Service\npublic class Named {\n  Named(Notifier pushNotifier) {}\n}\n",
        ),
    ]);
    assert_eq!(
        edges(&g, EdgeKind::Injects),
        [
            // The parameter name matches a bean name.
            "Named -> PushNotifier (0.81)",
            "Sender -> MailNotifier (0.40)",
            "Sender -> PushNotifier (0.40)",
        ]
    );
}

#[test]
fn spring_configuration_keys_and_profiles() {
    let g = graph(&[
        (
            "src/main/resources/application.yml",
            "app:\n  mail:\n    host: smtp\n    port: 25\nserver:\n  port: 8080\n",
        ),
        (
            "src/main/resources/application-dev.properties",
            "app.mail.host=localhost\napp.retry-count=3\n",
        ),
        (
            "src/test/resources/application.yml",
            "app:\n  mail:\n    host: test\n",
        ),
        (
            "src/main/java/app/Mail.java",
            r#"package app;
@Component
public class Mail {
    @Value("${app.mail.host}")
    private String host;
    Mail(@Value("${app.retryCount:2}") int retries) {}
}
@ConfigurationProperties(prefix = "app.mail")
class MailProperties {}
"#,
        ),
    ]);
    assert_eq!(
        edges(&g, EdgeKind::Configures),
        [
            "application-dev.properties -> Mail (0.90)",
            "application-dev.properties -> Mail.host (0.90)",
            "application-dev.properties -> MailProperties (0.90)",
            "application.yml -> Mail.host (0.90)",
            "application.yml -> MailProperties (0.90)",
        ]
    );
    assert_eq!(tag(&g, "Mail", "config.keys"), Some("app.retryCount"));
    assert_eq!(tag(&g, "MailProperties", "config.prefix"), Some("app.mail"));
    assert_eq!(
        tag(&g, "application-dev.properties", "spring.profile"),
        Some("dev")
    );
    assert!(g.symbol(sym(&g, "MailProperties")).has_role(Role::Config));
}

#[test]
fn spring_events_link_publishers_to_listeners() {
    let g = graph(&[
        (
            "src/main/java/app/Events.java",
            r#"package app;
class OrderEvent {}
class OrderPlaced extends OrderEvent {}
@Service
class Orders {
    private final ApplicationEventPublisher publisher;
    void place() { publisher.publishEvent(new OrderPlaced()); }
}
@Component
class Listeners {
    @EventListener
    void onPlaced(OrderPlaced e) {}
    @EventListener(OrderEvent.class)
    void onAny() {}
    @EventListener
    void onOther(String s) {}
}
"#,
        ),
        (
            "src/main/kotlin/app/Stock.kt",
            "package app\n@Service\nclass Stock(private val events: ApplicationEventPublisher) {\n  fun reserve() { events.publishEvent(OrderPlaced()) }\n}\n",
        ),
    ]);
    assert_eq!(
        edges(&g, EdgeKind::Publishes),
        [
            "Orders.place -> Listeners.onAny (0.85)",
            "Orders.place -> Listeners.onPlaced (0.85)",
            "Stock.reserve -> Listeners.onAny (0.85)",
            "Stock.reserve -> Listeners.onPlaced (0.85)",
        ]
    );
    assert_eq!(
        tag(&g, "Listeners.onPlaced", "event.type"),
        Some("OrderPlaced")
    );
    assert_eq!(
        tag(&g, "Orders.place", "event.publishes"),
        Some("OrderPlaced")
    );
}

#[test]
fn feign_clients_and_openapi_operations() {
    let g = graph(&[
        (
            "clients/src/main/java/app/client/OwnersClient.java",
            r#"package app.client;
@FeignClient(name = "owners", url = "${owners.url}")
public interface OwnersClient {
    @GetMapping("/api/owners/{id}")
    Owner get(@PathVariable("id") int id);
}
"#,
        ),
        (
            "owners/src/main/resources/openapi.yml",
            "openapi: 3.0.1\npaths:\n  /owners/{ownerId}:\n    get:\n      operationId: getOwner\n",
        ),
        (
            "owners/src/main/java/app/owners/OwnerController.java",
            r#"package app.owners;
@RestController
@RequestMapping("/api")
public class OwnerController implements OwnersApi {
    @Override
    public Owner getOwner(Integer ownerId) { return null; }
}
"#,
        ),
    ]);
    let client = g.symbol(sym(&g, "OwnersClient"));
    assert!(client.has_role(Role::External));
    assert_eq!(
        client.tags.get("http.client").map(String::as_str),
        Some("owners")
    );
    assert_eq!(
        tag(&g, "OwnersClient.get", "http.calls"),
        Some("GET /api/owners/{id}")
    );
    assert_eq!(
        tag(&g, "OwnerController.getOwner", "http.route"),
        Some("GET /api/owners/{ownerId}")
    );
    assert_eq!(
        edges(&g, EdgeKind::Configures),
        ["openapi.yml -> OwnerController.getOwner (0.80)"]
    );
    assert_eq!(
        edges(&g, EdgeKind::HttpCalls),
        ["OwnersClient.get -> OwnerController.getOwner (0.90)"]
    );
}

const APP_ROUTES: &str = r#"import { Routes } from '@angular/router';
import { HomeComponent } from './home.component';
import { ShellComponent } from './shell.component';
export const routes: Routes = [
  { path: '', component: HomeComponent },
  { path: 'app', component: ShellComponent, children: [
    { path: 'owners', loadChildren: () => import('./owners/owners.module').then(m => m.OwnersModule) },
    { path: 'about', loadComponent: () => import('./about.component').then(m => m.AboutComponent) },
  ]},
];
"#;
const OWNERS_MODULE: &str = r#"import { NgModule } from '@angular/core';
import { OwnersRoutingModule } from './owners-routing.module';
import { OwnerListComponent } from './owner-list.component';
@NgModule({
  declarations: [OwnerListComponent],
  imports: [OwnersRoutingModule],
})
export class OwnersModule {}
"#;
const OWNERS_ROUTING: &str = r#"import { NgModule } from '@angular/core';
import { RouterModule, Routes } from '@angular/router';
import { OwnerListComponent } from './owner-list.component';
const ownerRoutes: Routes = [{ path: ':id', component: OwnerListComponent }];
@NgModule({ imports: [RouterModule.forChild(ownerRoutes)] })
export class OwnersRoutingModule {}
"#;
const OWNER_LIST: &str = r#"import { Component } from '@angular/core';
import { OwnerService } from './owner.service';
@Component({ selector: 'app-owner-list', templateUrl: './owner-list.component.html' })
export class OwnerListComponent {
  constructor(private owners: OwnerService) {}
}
"#;
const OWNER_LIST_HTML: &str =
    "<h2>Owners</h2>\n<app-owner-row *ngFor=\"let o of owners\"></app-owner-row>\n";
const OWNER_ROW: &str = r#"import { Component, inject } from '@angular/core';
import { API_URL } from '../tokens';
@Component({ selector: 'app-owner-row', template: '<span>x</span>' })
export class OwnerRowComponent {
  private url = inject(API_URL);
}
"#;
const TOKENS: &str = "import { InjectionToken } from '@angular/core';\nexport const API_URL = new InjectionToken<string>('api');\n";
const OWNER_SERVICE: &str = r#"import { Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { environment } from '../../environments/environment';
@Injectable({ providedIn: 'root' })
export class OwnerService {
  private readonly base = environment.apiUrl + 'owners';
  constructor(private http: HttpClient) {}
  get(id: number) { return this.http.get<Owner>(`${this.base}/${id}`); }
  search(name: string) { return this.http.get<Owner[]>(this.base + '/search?name=' + name); }
  save(o: Owner) { return this.http.put(this.base + '/' + o.id, o); }
}
"#;
const ENVIRONMENT: &str = "export const environment = {\n  production: false,\n  apiUrl: 'http://localhost:8080/petclinic/api/'\n};\n";
const ENVIRONMENT_PROD: &str =
    "export const environment = {\n  production: true,\n  apiUrl: '/petclinic/api/'\n};\n";

fn angular_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("web/src/app/app.routes.ts", APP_ROUTES),
        (
            "web/src/app/home.component.ts",
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-home', template: '<app-owner-list></app-owner-list>' })\nexport class HomeComponent {}\n",
        ),
        (
            "web/src/app/shell.component.ts",
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-shell', template: '' })\nexport class ShellComponent {}\n",
        ),
        (
            "web/src/app/about.component.ts",
            "import { Component } from '@angular/core';\nimport { OwnerRowComponent } from './owners/owner-row.component';\n@Component({ selector: 'app-about', standalone: true, imports: [OwnerRowComponent], template: '' })\nexport class AboutComponent {}\n",
        ),
        ("web/src/app/owners/owners.module.ts", OWNERS_MODULE),
        (
            "web/src/app/owners/owners-routing.module.ts",
            OWNERS_ROUTING,
        ),
        ("web/src/app/owners/owner-list.component.ts", OWNER_LIST),
        (
            "web/src/app/owners/owner-list.component.html",
            OWNER_LIST_HTML,
        ),
        ("web/src/app/owners/owner-row.component.ts", OWNER_ROW),
        ("web/src/app/tokens.ts", TOKENS),
        ("web/src/app/owners/owner.service.ts", OWNER_SERVICE),
        ("web/src/environments/environment.ts", ENVIRONMENT),
        ("web/src/environments/environment.prod.ts", ENVIRONMENT_PROD),
    ]
}

#[test]
fn angular_routes_templates_and_injection() {
    let g = graph(&angular_files());
    let list = g.symbol(sym(&g, "OwnerListComponent"));
    assert!(list.has_role(Role::EntryPoint) && list.has_role(Role::View));
    // Lazy module prefix + parent route + own path.
    assert_eq!(
        list.tags.get("angular.route").map(String::as_str),
        Some("/app/owners/:id")
    );
    assert_eq!(tag(&g, "HomeComponent", "angular.route"), Some("/"));
    assert_eq!(
        tag(&g, "AboutComponent", "angular.route"),
        Some("/app/about")
    );
    assert_eq!(tag(&g, "OwnerService", "angular.provided_in"), Some("root"));
    assert_eq!(
        edges(&g, EdgeKind::Routes),
        [
            "HomeComponent -> OwnerListComponent (0.90)",
            "OwnerListComponent -> OwnerRowComponent (0.90)",
            "owner-list.component.html -> OwnerRowComponent (0.90)",
            // Imported component: 0.95 × 0.95.
            "ownerRoutes -> OwnerListComponent (0.90)",
            "routes -> AboutComponent (0.90)",
            "routes -> HomeComponent (0.90)",
            "routes -> OwnersModule (0.90)",
            "routes -> ShellComponent (0.90)",
        ]
    );
    assert_eq!(
        edges(&g, EdgeKind::Injects),
        [
            "OwnerListComponent -> OwnerService (0.85)",
            "OwnerRowComponent -> API_URL (0.90)",
        ]
    );
    let uses = edges(&g, EdgeKind::Uses);
    assert!(
        uses.contains(&"OwnersModule -> OwnerListComponent (0.90)".to_string()),
        "{uses:?}"
    );
    assert!(
        uses.contains(&"AboutComponent -> OwnerRowComponent (0.90)".to_string()),
        "{uses:?}"
    );
    assert!(uses.contains(&"OwnerListComponent -> owner-list.component.html (0.95)".to_string()));
    // Environments configure the service reading them.
    assert_eq!(
        edges(&g, EdgeKind::Configures),
        [
            "environment -> OwnerService (0.60)",
            "environment -> OwnerService (0.90)"
        ]
    );
    assert!(g.symbol(sym(&g, "environment.ts")).has_role(Role::Config));
}

const OWNER_REST: &str = r#"package app.owners;
@RestController
@RequestMapping("/api/owners")
public class OwnerRestController {
    @GetMapping("/{ownerId}")
    public Owner get(@PathVariable int ownerId) { return null; }
    @GetMapping("/search")
    public List<Owner> search(@RequestParam String name) { return null; }
    @PutMapping("/{ownerId}")
    public Owner update(@PathVariable int ownerId, @RequestBody Owner o) { return o; }
}
"#;

#[test]
fn http_calls_link_front_to_back() {
    let mut files = angular_files();
    files.push((
        "api/src/main/java/app/owners/OwnerRestController.java",
        OWNER_REST,
    ));
    files.push((
        "api/src/main/resources/application.properties",
        "server.servlet.context-path=/petclinic\n",
    ));
    let g = graph(&files);
    assert_eq!(
        tag(&g, "OwnerService.get", "http.calls"),
        Some("GET /petclinic/api/owners/*")
    );
    assert_eq!(
        tag(&g, "OwnerRestController.get", "http.context_path"),
        Some("/petclinic")
    );
    assert_eq!(
        edges(&g, EdgeKind::HttpCalls),
        [
            "OwnerService.get -> OwnerRestController.get (0.90)",
            // A literal segment prefers the literal route over `{ownerId}`.
            "OwnerService.save -> OwnerRestController.update (0.90)",
            "OwnerService.search -> OwnerRestController.search (0.90)",
        ]
    );
}

#[test]
fn http_calls_tolerate_prefix_differences_with_lower_confidence() {
    let g = graph(&[
        (
            "web/src/app/vet.service.ts",
            r#"import { HttpClient } from '@angular/common/http';
export class VetService {
  constructor(private http: HttpClient, private baseUrl: string) {}
  all() { return this.http.get(this.baseUrl + '/vets'); }
  one(id: number) { return this.http.get('/vets/' + id); }
  gone() { return this.http.delete('/vets/1'); }
}
"#,
        ),
        (
            "api/src/main/java/app/VetController.java",
            "package app;\n@RestController\n@RequestMapping(\"/api\")\nclass VetController {\n  @GetMapping(\"/vets\") List<Vet> all() { return null; }\n  @GetMapping(\"/vets/{id}\") Vet one(int id) { return null; }\n}\n",
        ),
    ]);
    assert_eq!(tag(&g, "VetService.all", "http.calls"), Some("GET */vets"));
    assert_eq!(
        edges(&g, EdgeKind::HttpCalls),
        [
            // Unknown base URL: any prefix.
            "VetService.all -> VetController.all (0.76)",
            // `/api` missing on the front: tolerated, lower.
            "VetService.one -> VetController.one (0.54)",
        ]
    );
}
