use remembrall_core::{
    graph::types::{RelationType, SymbolType},
    parser::{index_directory, index_directory_strict, IndexResult},
};
use std::{fs, path::PathBuf};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("remembrall-parser-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).unwrap();
    }
    fn index(&self) -> IndexResult {
        index_directory_strict(&self.0, "parser-fixture").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn has_edge(r: &IndexResult, from: &str, to_file: &str, to: &str, kind: RelationType) -> bool {
    r.relationships
        .iter()
        .filter(|e| e.rel_type == kind)
        .any(|e| {
            r.symbols
                .iter()
                .any(|s| s.id == e.source_id && s.name == from)
                && r.symbols
                    .iter()
                    .any(|s| s.id == e.target_id && s.name == to && s.file_path.ends_with(to_file))
        })
}

#[test]
fn unknown_map_receiver_never_fans_out_even_to_a_unique_method() {
    for n in [1, 8, 32] {
        let mut counts = vec![];
        for same in [true, false] {
            let f = Fixture::new();
            let mut callers = String::new();
            for i in 0..n {
                callers.push_str(&format!(
                    "function caller{i}(map: Map<string, number>) {{ return map.get('x'); }}\n"
                ));
                let method = if same {
                    "get".into()
                } else {
                    format!("peek{i}")
                };
                f.write(
                    &format!("target{i}.ts"),
                    &format!("export class C{i} {{ {method}() {{ return 1; }} }}"),
                );
            }
            f.write("callers.ts", &callers);
            let r = f.index();
            assert_eq!(r.files_parsed, n + 1);
            let ids: Vec<_> = r.symbols.iter().map(|s| s.id).collect();
            assert_eq!(
                r.relationships
                    .iter()
                    .filter(|e| e.rel_type == RelationType::Calls && ids.contains(&e.target_id))
                    .count(),
                0
            );
            assert_eq!(
                r.relationships
                    .iter()
                    .filter(|e| e.rel_type == RelationType::Imports)
                    .count(),
                0
            );
            counts.push(r.relationships.len());
        }
        assert_eq!(
            counts[0], counts[1],
            "unrelated names cannot alter dependencies for N={n}"
        );
    }
}

#[test]
fn imports_alias_namespace_default_and_types_resolve_to_the_declared_module() {
    let f = Fixture::new();
    f.write("dep.ts", "export function fetch() {}\nexport class Config {}\nexport default function primary() {}\n");
    f.write(
        "decoy.ts",
        "export function fetch() {}\nexport class Config {}\n",
    );
    f.write("caller.ts", "import { fetch as load, Config as Settings } from './dep';\nimport * as api from './dep';\nimport entry from './dep';\nfunction caller(x: Settings) { load(); api.fetch(); entry(); }\n");
    let r = f.index();
    assert!(has_edge(
        &r,
        "caller",
        "dep.ts",
        "fetch",
        RelationType::Calls
    ));
    assert!(has_edge(
        &r,
        "caller",
        "dep.ts",
        "primary",
        RelationType::Calls
    ));
    assert!(has_edge(
        &r,
        "caller",
        "dep.ts",
        "Config",
        RelationType::UsesType
    ));
    assert!(!has_edge(
        &r,
        "caller",
        "decoy.ts",
        "fetch",
        RelationType::Calls
    ));
    assert!(!has_edge(
        &r,
        "caller",
        "decoy.ts",
        "Config",
        RelationType::UsesType
    ));
    let resolved_calls: Vec<_> = r
        .relationships
        .iter()
        .filter(|e| e.rel_type == RelationType::Calls)
        .collect();
    assert_eq!(resolved_calls.len(), 3);
    assert!(resolved_calls
        .iter()
        .all(|e| r.symbols.iter().any(|s| s.id == e.target_id)));
}

#[test]
fn same_class_and_local_calls_survive_without_unknown_receiver_matches() {
    let f = Fixture::new();
    f.write("local.ts", "function helper() {}\nclass A { get() {} run(map: Map<string, number>) { this.get(); map.get('x'); helper(); } }\nclass B { get() {} run() { this.get(); } }\n");
    let r = f.index();
    for class in ["A", "B"] {
        let id = r.symbols.iter().find(|s| s.name == class).unwrap().id;
        let children: Vec<_> = r
            .relationships
            .iter()
            .filter(|e| e.rel_type == RelationType::Defines && e.source_id == id)
            .map(|e| e.target_id)
            .collect();
        let get = r
            .symbols
            .iter()
            .find(|s| s.name == "get" && children.contains(&s.id))
            .unwrap();
        let run = r
            .symbols
            .iter()
            .find(|s| s.name == "run" && children.contains(&s.id))
            .unwrap();
        assert!(r.relationships.iter().any(|e| e.source_id == run.id
            && e.target_id == get.id
            && e.rel_type == RelationType::Calls));
        assert_eq!(
            r.relationships
                .iter()
                .filter(|e| e.source_id == run.id
                    && e.rel_type == RelationType::Calls
                    && r.symbols
                        .iter()
                        .any(|s| s.id == e.target_id && s.name == "get"))
                .count(),
            1
        );
    }
    assert!(has_edge(
        &r,
        "run",
        "local.ts",
        "helper",
        RelationType::Calls
    ));
}

#[test]
fn parameter_shadowing_does_not_call_imported_or_local_function() {
    let f = Fixture::new();
    f.write("dep.ts", "export function fetch() {}\n");
    f.write("caller.ts", "import { fetch } from './dep';\nfunction helper() {}\nfunction caller(fetch: () => void, helper: () => void) { fetch(); helper(); }\n");
    let r = f.index();
    assert!(!has_edge(
        &r,
        "caller",
        "dep.ts",
        "fetch",
        RelationType::Calls
    ));
    assert!(!has_edge(
        &r,
        "caller",
        "caller.ts",
        "helper",
        RelationType::Calls
    ));
}

#[test]
fn python_import_alias_calls_and_types_have_module_evidence() {
    let f = Fixture::new();
    f.write(
        "dep.py",
        "def fetch():\n    pass\nclass Config:\n    pass\n",
    );
    f.write(
        "decoy.py",
        "def fetch():\n    pass\nclass Config:\n    pass\n",
    );
    f.write(
        "caller.py",
        "from .dep import fetch as load, Config\ndef caller(x: Config):\n    load()\n",
    );
    let r = f.index();
    assert!(has_edge(
        &r,
        "caller",
        "dep.py",
        "fetch",
        RelationType::Calls
    ));
    assert!(has_edge(
        &r,
        "caller",
        "dep.py",
        "Config",
        RelationType::UsesType
    ));
    assert!(!has_edge(
        &r,
        "caller",
        "decoy.py",
        "fetch",
        RelationType::Calls
    ));
}

#[test]
fn rust_import_alias_calls_and_types_have_module_evidence() {
    let f = Fixture::new();
    f.write("dep.rs", "pub fn fetch() {}\npub struct Config {}\n");
    f.write("decoy.rs", "pub fn fetch() {}\npub struct Config {}\n");
    f.write(
        "lib.rs",
        "use crate::dep::{fetch as load, Config};\nfn caller(x: Config) { load(); }\n",
    );
    let r = f.index();
    assert!(has_edge(
        &r,
        "caller",
        "dep.rs",
        "fetch",
        RelationType::Calls
    ));
    assert!(has_edge(
        &r,
        "caller",
        "dep.rs",
        "Config",
        RelationType::UsesType
    ));
    assert!(!has_edge(
        &r,
        "caller",
        "decoy.rs",
        "fetch",
        RelationType::Calls
    ));
}

#[test]
fn strict_scan_refuses_malformed_or_unreadable_source_but_permissive_api_remains() {
    let f = Fixture::new();
    f.write("bad.ts", "function broken( {");
    assert!(index_directory_strict(&f.0, "fixture")
        .unwrap_err()
        .to_string()
        .contains("bad.ts"));
    assert_eq!(
        index_directory(&f.0, "fixture", None).unwrap().files_parsed,
        1
    );
    fs::write(f.0.join("bad.ts"), [0xff, 0xfe]).unwrap();
    assert!(index_directory_strict(&f.0, "fixture").is_err());
    assert_eq!(
        index_directory(&f.0, "fixture", None).unwrap().files_parsed,
        0
    );
    assert!(index_directory_strict(f.0.join("absent"), "fixture").is_err());
}

#[test]
fn explicit_excluded_name_root_is_scanned_not_an_empty_publication() {
    let f = Fixture::new();
    for root in [".hidden", "build", "target"] {
        let dir = f.0.join(root);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("valid.ts"), "function valid() {}\n").unwrap();
        let r = index_directory_strict(&dir, "fixture").unwrap();
        assert_eq!(r.files_parsed, 1);
        assert!(r
            .symbols
            .iter()
            .any(|s| s.name == "valid" && s.symbol_type == SymbolType::Function));
    }
}

#[test]
fn lexical_shadows_and_dynamic_this_are_not_import_or_class_evidence() {
    let f = Fixture::new();
    f.write("dep.ts", "export function target() {}\n");
    f.write("caller.ts", "import { target } from './dep';\nfunction local() { const target = () => 1; return target(); }\nfunction value() { let target = external; target(); }\nfunction destructured({target}: any) { target(); }\nfunction caught() { try {} catch (target) { target(); } }\nclass C { get() {} run() { function dynamic() { this.get(); } const arrow = () => this.get(); } }\n");
    let r = f.index();
    for caller in ["local", "value", "destructured", "caught"] {
        assert!(
            !has_edge(&r, caller, "dep.ts", "target", RelationType::Calls),
            "shadow leaked for {caller}"
        );
    }
    assert!(has_edge(
        &r,
        "local",
        "caller.ts",
        "target",
        RelationType::Calls
    ));
    assert!(!has_edge(
        &r,
        "dynamic",
        "caller.ts",
        "get",
        RelationType::Calls
    ));
    assert!(has_edge(
        &r,
        "arrow",
        "caller.ts",
        "get",
        RelationType::Calls
    ));
}

#[test]
fn rust_imported_type_qualifies_methods_without_global_name_guess() {
    let f = Fixture::new();
    f.write(
        "dep.rs",
        "pub struct Config {}\nimpl Config { pub fn new() -> Self { Self {} } }\n",
    );
    f.write(
        "lib.rs",
        "use crate::dep::Config as Settings;\nfn caller() { Settings::new(); }\n",
    );
    let r = f.index();
    assert!(has_edge(
        &r,
        "caller",
        "dep.rs",
        "Config::new",
        RelationType::Calls
    ));
}

#[test]
fn ambiguous_module_suffix_and_missing_directory_entrypoint_do_not_guess() {
    let f = Fixture::new();
    for dir in ["one", "two", "pkg"] {
        fs::create_dir(f.0.join(dir)).unwrap();
    }
    f.write("one/dep.rs", "pub fn fetch() {}\n");
    f.write("two/dep.rs", "pub fn fetch() {}\n");
    f.write(
        "lib.rs",
        "use crate::dep::fetch;\nfn caller() { fetch(); }\n",
    );
    f.write("pkg/arbitrary.ts", "export function fetch() {}\n");
    f.write(
        "caller.ts",
        "import {fetch} from './pkg';\nfunction caller() {fetch();}\n",
    );
    let r = f.index();
    let ids: Vec<_> = r.symbols.iter().map(|s| s.id).collect();
    assert_eq!(
        r.relationships
            .iter()
            .filter(|e| e.rel_type == RelationType::Calls && ids.contains(&e.target_id))
            .count(),
        0
    );
}
