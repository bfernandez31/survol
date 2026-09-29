use std::collections::HashSet;

use super::*;

const M: &str = "api/src/main/java/fr/gouv/app";
const T: &str = "api/src/test/java/fr/gouv/app";

/// A module with main, test and openapi source sets, and a front-end.
fn paths() -> Vec<String> {
    [
        format!("{M}/auth/ContexteService.java"),
        format!("{M}/auth/Support.java"),
        format!("{M}/hab/application/style/StyleService.java"),
        format!("{M}/hab/application/style/StyleServiceImpl.java"),
        format!("{M}/hab/controller/StyleController.java"),
        format!("{M}/hab/dao/HabDao.java"),
        "api/src/main/openapi/api.json".to_string(),
        format!("{T}/hab/application/style/StyleServiceImplTest.java"),
        format!("{T}/hab/application/style/StyleServiceConcurrenceIT.java"),
        format!("{T}/hab/controller/StyleApiIT.java"),
        format!("{T}/hab/dao/HabDaoIT.java"),
        "front/e2e/jouables.json".to_string(),
        "front/e2e/params.spec.ts".to_string(),
        "front/src/app/app.routes.ts".to_string(),
    ]
    .to_vec()
}

fn files(paths: &[String]) -> Vec<(usize, &str)> {
    paths.iter().map(String::as_str).enumerate().collect()
}

/// One string per item: indent, then `label [note]` or `#file prefix`.
fn show(items: &[SideItem]) -> Vec<String> {
    items
        .iter()
        .map(|it| match it {
            SideItem::Dir {
                depth, label, note, ..
            } => {
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!(" [{note}]")
                };
                format!("{}{label}/{note}", "  ".repeat(*depth as usize))
            }
            SideItem::File {
                file,
                depth,
                prefix,
                role,
                place,
            } => {
                let role = match role {
                    Role::Plain => "",
                    Role::Class { tested: true } => " class",
                    Role::Class { tested: false } => " class, no test",
                    Role::Test { by_graph: false } => " test",
                    Role::Test { by_graph: true } => " test (graph)",
                };
                let place = if place.is_empty() {
                    String::new()
                } else {
                    format!(" @ {place}")
                };
                format!(
                    "{}{prefix}#{file}{role}{place}",
                    "  ".repeat(*depth as usize)
                )
            }
        })
        .collect()
}

#[test]
fn splits_source_sets_and_packages() {
    let p = split_path(&format!("{M}/hab/dao/HabDao.java"));
    assert_eq!(p.module, ["api"]);
    assert_eq!(p.set.as_deref(), Some("main"));
    assert!(p.code);
    assert_eq!(p.dirs, ["fr", "gouv", "app", "hab", "dao"]);
    assert_eq!(
        split_path("api/src/main/openapi/api.json").set.as_deref(),
        Some("openapi")
    );
    assert_eq!(
        split_path("api/src/test/resources/a.yml").set.as_deref(),
        Some("test/resources")
    );
    let front = split_path("front/src/app/app.routes.ts");
    assert_eq!(front.set, None);
    assert_eq!(front.dirs, ["front", "src", "app"]);
    assert!(is_test(&format!("{T}/X.java")));
    assert!(is_test("front/e2e/params.spec.ts"));
    assert!(is_test("lib/FooTest.kt"));
    assert!(!is_test(&format!("{M}/Testing.java")));
}

#[test]
fn compact_tree_with_source_sets_and_package_root() {
    let paths = paths();
    let items = tree(&files(&paths), &HashSet::new());
    assert_eq!(
        show(&items),
        [
            "api/",
            "  main/ [fr.gouv.app]",
            "    auth/",
            "      #0",
            "      #1",
            "    hab/",
            "      application/style/",
            "        #2",
            "        #3",
            "      controller/#4",
            "      dao/#5",
            "  openapi/",
            "    #6",
            "  test/ [same package]",
            "    hab/",
            "      application/style/",
            "        #7",
            "        #8",
            "      controller/#9",
            "      dao/#10",
            "front/",
            "  e2e/",
            "    #11",
            "    #12",
            "  src/app/#13",
        ]
    );
    // Each directory knows its files.
    let SideItem::Dir {
        files: under, key, ..
    } = &items[5]
    else {
        panic!("hab");
    };
    assert_eq!(under, &[2, 3, 4, 5]);
    // Folded: its content hidden, the rest kept.
    let folded: HashSet<String> = [key.clone(), "front/".to_string()].into();
    assert_eq!(
        show(&tree(&files(&paths), &folded))[4..7],
        [
            "      #1".to_string(),
            "    hab/".into(),
            "  openapi/".into()
        ]
    );
    assert!(show(&tree(&files(&paths), &folded)).ends_with(&["front/".to_string()]));
    assert!(dir_keys(&files(&paths)).contains("front/"));
}

#[test]
fn a_plain_repository_is_a_compact_tree() {
    let paths: Vec<String> = ["src/a/b/c/x.rs", "src/a/b/c/y.rs", "README.md"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        show(&tree(&files(&paths), &HashSet::new())),
        ["src/a/b/c/", "  #0", "  #1", "#2"]
    );
}

#[test]
fn pairs_classes_with_their_tests() {
    let paths = paths();
    // The graph says StyleApiIT tests the controller.
    let items = pairs(&files(&paths), |t| (t == 9).then_some(4));
    assert_eq!(
        show(&items),
        [
            "auth/",
            "  #0 class, no test",
            "  #1 class, no test",
            "hab/application/style/",
            "  #2 class",
            "    #8 test",
            "  #3 class",
            "    #7 test",
            "hab/controller/",
            "  #4 class",
            "    #9 test (graph)",
            "hab/dao/",
            "  #5 class",
            "    #10 test",
            "openapi/",
            "  #6",
            "front/e2e/",
            "  #11",
            "  #12",
            "front/src/app/",
            "  #13 class, no test",
        ]
    );
    // Without the graph, the unmatched test stays on its own.
    let items = pairs(&files(&paths), |_| None);
    assert!(show(&items).contains(&"  #9".to_string()));
    // A pairs directory folds like a tree one.
    let SideItem::Dir { key, .. } = &items[0] else {
        panic!("a directory first");
    };
    let folded = fold_flat(items.clone(), &[key.clone()].into());
    assert_eq!(show(&folded)[..2], ["auth/", "hab/application/style/"]);
}

#[test]
fn flat_list_names_first_place_after() {
    let paths = paths();
    let items = flat(&files(&paths));
    let s = show(&items);
    assert_eq!(s[0], "#0 @ main · auth");
    assert_eq!(s[6], "#6 @ openapi");
    assert_eq!(s[7], "#7 @ test · hab/application/style");
    assert_eq!(s[11], "#11 @ front/e2e");
    assert_eq!(Mode::parse("pairs"), Some(Mode::Pairs));
    assert_eq!(Mode::Flat.next(), Mode::Tree);
}
